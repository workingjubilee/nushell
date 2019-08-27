use crate::commands::WholeStreamCommand;
use crate::object::{Dictionary, Primitive, Value};
use crate::prelude::*;
use hex::encode;
use rusqlite::{Connection, NO_PARAMS};
use std::io::Read;

pub struct ToSQLite;

impl WholeStreamCommand for ToSQLite {
    fn run(
        &self,
        args: CommandArgs,
        registry: &CommandRegistry,
    ) -> Result<OutputStream, ShellError> {
        to_sqlite(args, registry)
    }

    fn name(&self) -> &str {
        "to-sqlite"
    }

    fn signature(&self) -> Signature {
        Signature::build("to-sqlite")
    }
}

fn comma_concat(acc: String, current: String) -> String {
    if acc == "" {
        current
    } else {
        format!("{}, {}", acc, current)
    }
}

fn get_columns(rows: &Vec<Tagged<Value>>) -> Result<String, std::io::Error> {
    match &rows[0].item {
        Value::Object(d) => Ok(d
            .entries
            .iter()
            .map(|(k, _v)| k.clone())
            .fold("".to_string(), comma_concat)),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Could not find table column names",
        )),
    }
}

fn nu_value_to_sqlite_string(v: Value) -> String {
    match v {
        Value::Binary(u) => format!("x'{}'", encode(u)),
        Value::Primitive(p) => match p {
            Primitive::Nothing => "NULL".into(),
            Primitive::Int(i) => format!("{}", i),
            Primitive::Float(f) => format!("{}", f.into_inner()),
            Primitive::Bytes(u) => format!("{}", u),
            Primitive::String(s) => format!("'{}'", s.replace("'", "''")),
            Primitive::Boolean(true) => "1".into(),
            Primitive::Boolean(_) => "0".into(),
            Primitive::Date(d) => format!("'{}'", d),
            Primitive::Path(p) => format!("'{}'", p.display().to_string().replace("'", "''")),
            Primitive::BeginningOfStream => "NULL".into(),
            Primitive::EndOfStream => "NULL".into(),
        },
        _ => "NULL".into(),
    }
}

fn get_insert_values(rows: Vec<Tagged<Value>>) -> Result<String, std::io::Error> {
    let values: Result<Vec<_>, _> = rows
        .into_iter()
        .map(|value| match value.item {
            Value::Object(d) => Ok(format!(
                "({})",
                d.entries
                    .iter()
                    .map(|(_k, v)| nu_value_to_sqlite_string(v.item.clone()))
                    .fold("".to_string(), comma_concat)
            )),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Could not find table column names",
            )),
        })
        .collect();
    let values = values?;
    Ok(values.into_iter().fold("".to_string(), comma_concat))
}

fn generate_statements(table: Dictionary) -> Result<(String, String), std::io::Error> {
    let table_name = match table.entries.get("table_name") {
        Some(Tagged {
            item: Value::Primitive(Primitive::String(table_name)),
            ..
        }) => table_name,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Could not find table name",
            ))
        }
    };
    let (columns, insert_values) = match table.entries.get("table_values") {
        Some(Tagged {
            item: Value::List(l),
            ..
        }) => (get_columns(l), get_insert_values(l.to_vec())),
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Could not find table values",
            ))
        }
    };
    let create = format!("create table {}({})", table_name, columns?);
    let insert = format!("insert into {} values {}", table_name, insert_values?);
    Ok((create, insert))
}

fn sqlite_input_stream_to_bytes(
    values: Vec<Tagged<Value>>,
) -> Result<Tagged<Value>, std::io::Error> {
    // FIXME: should probably write a sqlite virtual filesystem
    // that will allow us to use bytes as a file to avoid this
    // write out, but this will require C code. Might be
    // best done as a PR to rusqlite.
    let mut tempfile = tempfile::NamedTempFile::new()?;
    let conn = match Connection::open(tempfile.path()) {
        Ok(conn) => conn,
        Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    };
    let tag = values[0].tag.clone();
    for value in values.into_iter() {
        match value.item() {
            Value::Object(d) => {
                let (create, insert) = generate_statements(d.to_owned())?;
                match conn
                    .execute(&create, NO_PARAMS)
                    .and_then(|_| conn.execute(&insert, NO_PARAMS))
                {
                    Ok(_) => (),
                    Err(e) => {
                        println!("{}", create);
                        println!("{}", insert);
                        println!("{:?}", e);
                        return Err(std::io::Error::new(std::io::ErrorKind::Other, e));
                    }
                }
            }
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Expected object, found {:?}", other),
                ))
            }
        }
    }
    let mut out = Vec::new();
    tempfile.read_to_end(&mut out)?;
    Ok(Value::Binary(out).tagged(tag))
}

fn to_sqlite(args: CommandArgs, registry: &CommandRegistry) -> Result<OutputStream, ShellError> {
    let args = args.evaluate_once(registry)?;
    let name_span = args.name_span();
    let stream = async_stream_block! {
        let values: Vec<_> = args.input.into_vec().await;
        match sqlite_input_stream_to_bytes(values) {
            Ok(out) => {
                yield ReturnSuccess::value(out)
            }
            Err(_) => {
                yield Err(ShellError::labeled_error(
                    "Expected an object with SQLite-compatible structure from pipeline",
                    "requires SQLite-compatible input",
                    name_span,
                    ))
            }
        };
    };
    Ok(stream.to_output_stream())
}
