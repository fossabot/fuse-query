// Copyright 2020-2021 The FuseQuery Authors.
//
// Code is licensed under AGPL License, Version 3.0.
//
// Borrow from apache/arrow/rust/datafusion/src/sql/sql_parser
// See NOTICE.md

use crate::planners::DFExplainType;
use sqlparser::{
    ast::{ColumnDef, ColumnOptionDef, Statement as SQLStatement, TableConstraint},
    dialect::{keywords::Keyword, Dialect, GenericDialect},
    parser::{Parser, ParserError},
    tokenizer::{Token, Tokenizer},
};

// Use `Parser::expected` instead, if possible
macro_rules! parser_err {
    ($MSG:expr) => {
        Err(ParserError::ParserError($MSG.to_string()))
    };
}

/// Types of files to parse as DataFrames
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileType {
    /// Newline-delimited JSON
    NdJson,
    /// Apache Parquet columnar storage
    Parquet,
    /// Comma separated values
    CSV,
}

/// DataFusion extension DDL for `CREATE EXTERNAL TABLE`
#[derive(Debug, Clone, PartialEq)]
pub struct CreateExternalTable {
    /// Table name
    pub name: String,
    /// Optional schema
    pub columns: Vec<ColumnDef>,
    /// File type (Parquet, NDJSON, CSV)
    pub file_type: FileType,
    /// CSV Header row?
    pub has_header: bool,
    /// Path to file
    pub location: String,
}

/// DataFusion extension DDL for `EXPLAIN` and `EXPLAIN VERBOSE`
#[derive(Debug, Clone, PartialEq)]
pub struct DFExplainPlan {
    pub typ: DFExplainType,
    /// The statement for which to generate an planning explanation
    pub statement: Box<SQLStatement>,
}

/// DataFusion Statement representations.
///
/// Tokens parsed by `DFParser` are converted into these values.
#[derive(Debug, Clone, PartialEq)]
pub enum DFStatement {
    /// ANSI SQL AST node
    Statement(SQLStatement),
    /// Extension: `CREATE EXTERNAL TABLE`
    CreateExternalTable(CreateExternalTable),
    /// Extension: `EXPLAIN <SQL>`
    Explain(DFExplainPlan),
}

/// SQL Parser
pub struct DFParser<'a> {
    parser: Parser<'a>,
}

impl<'a> DFParser<'a> {
    /// Parse the specified tokens
    pub fn new(sql: &str) -> Result<Self, ParserError> {
        let dialect = &GenericDialect {};
        DFParser::new_with_dialect(sql, dialect)
    }

    /// Parse the specified tokens with dialect
    pub fn new_with_dialect(sql: &str, dialect: &'a dyn Dialect) -> Result<Self, ParserError> {
        let mut tokenizer = Tokenizer::new(dialect, sql);
        let tokens = tokenizer.tokenize()?;

        Ok(DFParser {
            parser: Parser::new(tokens, dialect),
        })
    }

    /// Parse a SQL statement and produce a set of statements with dialect
    pub fn parse_sql(sql: &str) -> Result<Vec<DFStatement>, ParserError> {
        let dialect = &GenericDialect {};
        DFParser::parse_sql_with_dialect(sql, dialect)
    }

    /// Parse a SQL statement and produce a set of statements
    pub fn parse_sql_with_dialect(
        sql: &str,
        dialect: &dyn Dialect,
    ) -> Result<Vec<DFStatement>, ParserError> {
        let mut parser = DFParser::new_with_dialect(sql, dialect)?;
        let mut stmts = Vec::new();
        let mut expecting_statement_delimiter = false;
        loop {
            // ignore empty statements (between successive statement delimiters)
            while parser.parser.consume_token(&Token::SemiColon) {
                expecting_statement_delimiter = false;
            }

            if parser.parser.peek_token() == Token::EOF {
                break;
            }
            if expecting_statement_delimiter {
                return parser.expected("end of statement", parser.parser.peek_token());
            }

            let statement = parser.parse_statement()?;
            stmts.push(statement);
            expecting_statement_delimiter = true;
        }
        Ok(stmts)
    }

    /// Report unexpected token
    fn expected<T>(&self, expected: &str, found: Token) -> Result<T, ParserError> {
        parser_err!(format!("Expected {}, found: {}", expected, found))
    }

    /// Parse a new expression
    pub fn parse_statement(&mut self) -> Result<DFStatement, ParserError> {
        match self.parser.peek_token() {
            Token::Word(w) => {
                match w.keyword {
                    Keyword::CREATE => {
                        // move one token forward
                        self.parser.next_token();
                        // use custom parsing
                        self.parse_create()
                    }
                    Keyword::EXPLAIN => {
                        self.parser.next_token();
                        self.parse_explain()
                    }
                    _ => {
                        // use the native parser
                        Ok(DFStatement::Statement(self.parser.parse_statement()?))
                    }
                }
            }
            _ => {
                // use the native parser
                Ok(DFStatement::Statement(self.parser.parse_statement()?))
            }
        }
    }

    /// Parse an SQL EXPLAIN statement.
    pub fn parse_explain(&mut self) -> Result<DFStatement, ParserError> {
        // Parser is at the token immediately after EXPLAIN
        // Check for EXPLAIN VERBOSE
        let typ = match self.parser.peek_token() {
            Token::Word(w) => match w.value.to_uppercase().as_str() {
                "PIPELINE" => {
                    self.parser.next_token();
                    DFExplainType::Pipeline
                }
                "GRAPH" => {
                    self.parser.next_token();
                    DFExplainType::Graph
                }
                _ => DFExplainType::Syntax,
            },
            _ => DFExplainType::Syntax,
        };

        let statement = Box::new(self.parser.parse_statement()?);
        let explain_plan = DFExplainPlan { statement, typ };
        Ok(DFStatement::Explain(explain_plan))
    }

    /// Parse a SQL CREATE statement
    pub fn parse_create(&mut self) -> Result<DFStatement, ParserError> {
        if self.parser.parse_keyword(Keyword::EXTERNAL) {
            self.parse_create_external_table()
        } else {
            Ok(DFStatement::Statement(self.parser.parse_create()?))
        }
    }

    // This is a copy of the equivalent implementation in sqlparser.
    fn parse_columns(&mut self) -> Result<(Vec<ColumnDef>, Vec<TableConstraint>), ParserError> {
        let mut columns = vec![];
        let mut constraints = vec![];
        if !self.parser.consume_token(&Token::LParen) || self.parser.consume_token(&Token::RParen) {
            return Ok((columns, constraints));
        }

        loop {
            if let Some(constraint) = self.parser.parse_optional_table_constraint()? {
                constraints.push(constraint);
            } else if let Token::Word(_) = self.parser.peek_token() {
                let column_def = self.parse_column_def()?;
                columns.push(column_def);
            } else {
                return self.expected(
                    "column name or constraint definition",
                    self.parser.peek_token(),
                );
            }
            let comma = self.parser.consume_token(&Token::Comma);
            if self.parser.consume_token(&Token::RParen) {
                // allow a trailing comma, even though it's not in standard
                break;
            } else if !comma {
                return self.expected(
                    "',' or ')' after column definition",
                    self.parser.peek_token(),
                );
            }
        }

        Ok((columns, constraints))
    }

    fn parse_column_def(&mut self) -> Result<ColumnDef, ParserError> {
        let name = self.parser.parse_identifier()?;
        let data_type = self.parser.parse_data_type()?;
        let collation = if self.parser.parse_keyword(Keyword::COLLATE) {
            Some(self.parser.parse_object_name()?)
        } else {
            None
        };
        let mut options = vec![];
        loop {
            if self.parser.parse_keyword(Keyword::CONSTRAINT) {
                let name = Some(self.parser.parse_identifier()?);
                if let Some(option) = self.parser.parse_optional_column_option()? {
                    options.push(ColumnOptionDef { name, option });
                } else {
                    return self.expected(
                        "constraint details after CONSTRAINT <name>",
                        self.parser.peek_token(),
                    );
                }
            } else if let Some(option) = self.parser.parse_optional_column_option()? {
                options.push(ColumnOptionDef { name: None, option });
            } else {
                break;
            };
        }
        Ok(ColumnDef {
            name,
            data_type,
            collation,
            options,
        })
    }

    fn parse_create_external_table(&mut self) -> Result<DFStatement, ParserError> {
        self.parser.expect_keyword(Keyword::TABLE)?;
        let table_name = self.parser.parse_object_name()?;
        let (columns, _) = self.parse_columns()?;
        self.parser
            .expect_keywords(&[Keyword::STORED, Keyword::AS])?;

        // THIS is the main difference: we parse a different file format.
        let file_type = self.parse_file_format()?;

        let has_header = self.parse_csv_has_header();

        self.parser.expect_keyword(Keyword::LOCATION)?;
        let location = self.parser.parse_literal_string()?;

        let create = CreateExternalTable {
            name: table_name.to_string(),
            columns,
            file_type,
            has_header,
            location,
        };
        Ok(DFStatement::CreateExternalTable(create))
    }

    /// Parses the set of valid formats
    fn parse_file_format(&mut self) -> Result<FileType, ParserError> {
        match self.parser.next_token() {
            Token::Word(w) => match &*w.value {
                "PARQUET" => Ok(FileType::Parquet),
                "NDJSON" => Ok(FileType::NdJson),
                "CSV" => Ok(FileType::CSV),
                _ => self.expected("one of PARQUET, NDJSON, or CSV", Token::Word(w)),
            },
            unexpected => self.expected("one of PARQUET, NDJSON, or CSV", unexpected),
        }
    }

    fn consume_token(&mut self, expected: &str) -> bool {
        if self.parser.peek_token().to_string() == *expected {
            self.parser.next_token();
            true
        } else {
            false
        }
    }

    fn parse_csv_has_header(&mut self) -> bool {
        self.consume_token("WITH") & self.consume_token("HEADER") & self.consume_token("ROW")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::{DataType, Ident};

    fn expect_parse_ok(sql: &str, expected: DFStatement) -> Result<(), ParserError> {
        let statements = DFParser::parse_sql(sql)?;
        assert_eq!(
            statements.len(),
            1,
            "Expected to parse exactly one statement"
        );
        assert_eq!(statements[0], expected);
        Ok(())
    }

    /// Parses sql and asserts that the expected error message was found
    fn expect_parse_error(sql: &str, expected_error: &str) -> Result<(), ParserError> {
        match DFParser::parse_sql(sql) {
            Ok(statements) => {
                panic!(
                    "Expected parse error for '{}', but was successful: {:?}",
                    sql, statements
                );
            }
            Err(e) => {
                let error_message = e.to_string();
                assert!(
                    error_message.contains(expected_error),
                    "Expected error '{}' not found in actual error '{}'",
                    expected_error,
                    error_message
                );
            }
        }
        Ok(())
    }

    fn make_column_def(name: impl Into<String>, data_type: DataType) -> ColumnDef {
        ColumnDef {
            name: Ident {
                value: name.into(),
                quote_style: None,
            },
            data_type,
            collation: None,
            options: vec![],
        }
    }

    #[test]
    fn create_external_table() -> Result<(), ParserError> {
        // positive case
        let sql = "CREATE EXTERNAL TABLE t(c1 int) STORED AS CSV LOCATION 'foo.csv'";
        let expected = DFStatement::CreateExternalTable(CreateExternalTable {
            name: "t".into(),
            columns: vec![make_column_def("c1", DataType::Int)],
            file_type: FileType::CSV,
            has_header: false,
            location: "foo.csv".into(),
        });
        expect_parse_ok(sql, expected)?;

        // positive case: it is ok for parquet files not to have columns specified
        let sql = "CREATE EXTERNAL TABLE t STORED AS PARQUET LOCATION 'foo.parquet'";
        let expected = DFStatement::CreateExternalTable(CreateExternalTable {
            name: "t".into(),
            columns: vec![],
            file_type: FileType::Parquet,
            has_header: false,
            location: "foo.parquet".into(),
        });
        expect_parse_ok(sql, expected)?;

        // Error cases: Invalid type
        let sql = "CREATE EXTERNAL TABLE t(c1 int) STORED AS UNKNOWN_TYPE LOCATION 'foo.csv'";
        expect_parse_error(
            sql,
            "Expected one of PARQUET, NDJSON, or CSV, found: UNKNOWN_TYPE",
        )?;

        Ok(())
    }
}
