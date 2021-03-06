// Copyright 2017 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

use std::collections::HashMap;
use std::io::Write;
use std::process;

use tabwriter::TabWriter;

use mentat::{
    Queryable,
    QueryExplanation,
    QueryOutput,
    QueryResults,
    Store,
    TxReport,
    TypedValue,
};

use command_parser::{
    Command,
    HELP_COMMAND,
    OPEN_COMMAND,
    LONG_QUERY_COMMAND,
    SHORT_QUERY_COMMAND,
    SCHEMA_COMMAND,
    LONG_TRANSACT_COMMAND,
    SHORT_TRANSACT_COMMAND,
    LONG_EXIT_COMMAND,
    SHORT_EXIT_COMMAND,
    LONG_QUERY_EXPLAIN_COMMAND,
    SHORT_QUERY_EXPLAIN_COMMAND,
};

use input::InputReader;
use input::InputResult::{
    MetaCommand,
    Empty,
    More,
    Eof,
};

lazy_static! {
    static ref COMMAND_HELP: HashMap<&'static str, &'static str> = {
        let mut map = HashMap::new();
        map.insert(LONG_EXIT_COMMAND, "Close the current database and exit the REPL.");
        map.insert(SHORT_EXIT_COMMAND, "Shortcut for `.exit`. Close the current database and exit the REPL.");
        map.insert(HELP_COMMAND, "Show help for commands.");
        map.insert(OPEN_COMMAND, "Open a database at path.");
        map.insert(LONG_QUERY_COMMAND, "Execute a query against the current open database.");
        map.insert(SHORT_QUERY_COMMAND, "Shortcut for `.query`. Execute a query against the current open database.");
        map.insert(SCHEMA_COMMAND, "Output the schema for the current open database.");
        map.insert(LONG_TRANSACT_COMMAND, "Execute a transact against the current open database.");
        map.insert(SHORT_TRANSACT_COMMAND, "Shortcut for `.transact`. Execute a transact against the current open database.");
        map.insert(LONG_QUERY_EXPLAIN_COMMAND, "Show the SQL and query plan that would be executed for a given query.");
        map.insert(SHORT_QUERY_EXPLAIN_COMMAND,
            "Shortcut for `.explain_query`. Show the SQL and query plan that would be executed for a given query.");
        map
    };
}

/// Executes input and maintains state of persistent items.
pub struct Repl {
    path: String,
    store: Store,
}

impl Repl {
    pub fn db_name(&self) -> String {
        if self.path.is_empty() {
            "in-memory db".to_string()
        } else {
            self.path.clone()
        }
    }

    /// Constructs a new `Repl`.
    pub fn new() -> Result<Repl, String> {
        let store = Store::open("").map_err(|e| e.to_string())?;
        Ok(Repl{
            path: "".to_string(),
            store: store,
        })
    }

    /// Runs the REPL interactively.
    pub fn run(&mut self, startup_commands: Option<Vec<Command>>) {
        let mut input = InputReader::new();

        if let Some(cmds) = startup_commands {
            for command in cmds.iter() {
                println!("{}", command.output());
                self.handle_command(command.clone());
            }
        }

        loop {
            let res = input.read_input();

            match res {
                Ok(MetaCommand(cmd)) => {
                    debug!("read command: {:?}", cmd);
                    self.handle_command(cmd);
                },
                Ok(Empty) |
                Ok(More) => (),
                Ok(Eof) => {
                    if input.is_tty() {
                        println!("");
                    }
                    break;
                },
                Err(e) => eprintln!("{}", e.to_string()),
            }
        }
    }

    /// Runs a single command input.
    fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Help(args) => self.help_command(args),
            Command::Open(db) => {
                match self.open(db) {
                    Ok(_) => println!("Database {:?} opened", self.db_name()),
                    Err(e) => eprintln!("{}", e.to_string()),
                };
            },
            Command::Close => self.close(),
            Command::Query(query) => self.execute_query(query),
            Command::QueryExplain(query) => self.explain_query(query),
            Command::Schema => {
                let edn = self.store.conn().current_schema().to_edn_value();
                match edn.to_pretty(120) {
                    Ok(s) => println!("{}", s),
                    Err(e) => eprintln!("{}", e)
                };

            }
            Command::Transact(transaction) => self.execute_transact(transaction),
            Command::Exit => {
                self.close();
                eprintln!("Exiting...");
                process::exit(0);
            }
        }
    }

    fn open<T>(&mut self, path: T) -> ::mentat::errors::Result<()>
    where T: Into<String> {
        let path = path.into();
        if self.path.is_empty() || path != self.path {
            let next = Store::open(path.as_str())?;
            self.path = path;
            self.store = next;
        }

        Ok(())
    }

    // Close the current store by opening a new in-memory store in its place.
    fn close(&mut self) {
        let old_db_name = self.db_name();
        match self.open("") {
            Ok(_) => println!("Database {:?} closed.", old_db_name),
            Err(e) => eprintln!("{}", e),
        };
    }

    fn help_command(&self, args: Vec<String>) {
        if args.is_empty() {
            for (cmd, msg) in COMMAND_HELP.iter() {
                println!(".{} - {}", cmd, msg);
            }
        } else {
            for mut arg in args {
                if arg.chars().nth(0).unwrap() == '.' {
                    arg.remove(0);
                }
                let msg = COMMAND_HELP.get(arg.as_str());
                if msg.is_some() {
                    println!(".{} - {}", arg, msg.unwrap());
                } else {
                    eprintln!("Unrecognised command {}", arg);
                }
            }
        }
    }

    pub fn execute_query(&self, query: String) {
        self.store.q_once(query.as_str(), None)
            .map_err(|e| e.into())
            .and_then(|o| self.print_results(o))
            .map_err(|err| {
                eprintln!("{:?}.", err);
            }).ok();
    }

    fn print_results(&self, query_output: QueryOutput) -> Result<(), ::errors::Error> {
        let stdout = ::std::io::stdout();
        let mut output = TabWriter::new(stdout.lock());

        // Print the column headers.
        for e in query_output.spec.columns() {
            write!(output, "| {}\t", e)?;
        }
        writeln!(output, "|")?;
        for _ in 0..query_output.spec.expected_column_count() {
            write!(output, "---\t")?;
        }
        writeln!(output, "")?;

        match query_output.results {
            QueryResults::Scalar(v) => {
                if let Some(val) = v {
                    writeln!(output, "| {}\t |", &self.typed_value_as_string(val))?;
                }
            },

            QueryResults::Tuple(vv) => {
                if let Some(vals) = vv {
                    for val in vals {
                        write!(output, "| {}\t", self.typed_value_as_string(val))?;
                    }
                    writeln!(output, "|")?;
                }
            },

            QueryResults::Coll(vv) => {
                for val in vv {
                    writeln!(output, "| {}\t|", self.typed_value_as_string(val))?;
                }
            },

            QueryResults::Rel(vvv) => {
                for vv in vvv {
                    for v in vv {
                        write!(output, "| {}\t", self.typed_value_as_string(v))?;
                    }
                    writeln!(output, "|")?;
                }
            },
        }
        for _ in 0..query_output.spec.expected_column_count() {
            write!(output, "---\t")?;
        }
        writeln!(output, "")?;
        output.flush()?;
        Ok(())
    }

    pub fn explain_query(&self, query: String) {
        match self.store.q_explain(query.as_str(), None) {
            Result::Err(err) =>
                println!("{:?}.", err),
            Result::Ok(QueryExplanation::KnownEmpty(empty_because)) =>
                println!("Query is known empty: {:?}", empty_because),
            Result::Ok(QueryExplanation::ExecutionPlan { query, steps }) => {
                println!("SQL: {}", query.sql);
                if !query.args.is_empty() {
                    println!("  Bindings:");
                    for (arg_name, value) in query.args {
                        println!("    {} = {:?}", arg_name, *value)
                    }
                }

                println!("Plan: select id | order | from | detail");
                // Compute the number of columns we need for order, select id, and from,
                // so that longer query plans don't become misaligned.
                let (max_select_id, max_order, max_from) = steps.iter().fold((0, 0, 0), |acc, step|
                    (acc.0.max(step.select_id), acc.1.max(step.order), acc.2.max(step.from)));
                // This is less efficient than computing it via the logarithm base 10,
                // but it's clearer and doesn't have require special casing "0"
                let max_select_digits = max_select_id.to_string().len();
                let max_order_digits = max_order.to_string().len();
                let max_from_digits = max_from.to_string().len();
                for step in steps {
                    // Note: > is right align.
                    println!("  {:>sel_cols$}|{:>ord_cols$}|{:>from_cols$}|{}",
                             step.select_id, step.order, step.from, step.detail,
                             sel_cols = max_select_digits,
                             ord_cols = max_order_digits,
                             from_cols = max_from_digits);
                }
            }
        };
    }

    pub fn execute_transact(&mut self, transaction: String) {
        match self.transact(transaction) {
            Result::Ok(report) => println!("{:?}", report),
            Result::Err(err) => eprintln!("Error: {:?}.", err),
        }
    }

    fn transact(&mut self, transaction: String) -> ::mentat::errors::Result<TxReport> {
        let mut tx = self.store.begin_transaction()?;
        let report = tx.transact(&transaction)?;
        tx.commit()?;
        Ok(report)
    }

    fn typed_value_as_string(&self, value: TypedValue) -> String {
        match value {
            TypedValue::Boolean(b) => if b { "true".to_string() } else { "false".to_string() },
            TypedValue::Double(d) => format!("{}", d),
            TypedValue::Instant(i) => format!("{}", i),
            TypedValue::Keyword(k) => format!("{}", k),
            TypedValue::Long(l) => format!("{}", l),
            TypedValue::Ref(r) => format!("{}", r),
            TypedValue::String(s) => format!("{:?}", s.to_string()),
            TypedValue::Uuid(u) => format!("{}", u),
        }
    }
}
