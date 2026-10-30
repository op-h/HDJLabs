//! Built-in interpreters, so Python and JavaScript questions work on machines with nothing
//! installed. Invoked as `HDJLabs.exe --exec <python|js> <file>` inside the runner sandbox,
//! which keeps infinite loops and crashes away from the exam UI.

use std::io::{BufRead, Write};

pub fn exec_main(args: &[String]) -> i32 {
    let (Some(lang), Some(path)) = (args.first(), args.get(1)) else {
        eprintln!("usage: --exec <python|js> <file>");
        return 2;
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return 2;
        }
    };
    match lang.as_str() {
        "python" => run_python(&src),
        "js" => run_js(&src),
        "sql" => run_sql(&src),
        _ => {
            eprintln!("unknown language {lang}");
            2
        }
    }
}

fn run_python(src: &str) -> i32 {
    let interp = rustpython::InterpreterConfig::new().init_stdlib().interpreter();
    let code = interp.run(|vm| {
        let scope = vm.new_scope_with_builtins();
        vm.compile(src, rustpython_vm::compiler::Mode::Exec, "main.py".to_owned())
            .map_err(|e| vm.new_syntax_error(&e, Some(src)))
            .and_then(|c| vm.run_code_obj(c, scope))
            .map(|_| ())
    });
    code as i32
}

/// Runs the database setup from stdin, then each statement of the student's query; every
/// result set is printed as `col | col` lines with a header row.
fn run_sql(src: &str) -> i32 {
    use std::io::Read;
    let mut setup = String::new();
    let _ = std::io::stdin().read_to_string(&mut setup);
    let conn = match rusqlite::Connection::open_in_memory() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SQLite: {e}");
            return 1;
        }
    };
    if let Err(e) = conn.execute_batch(&setup) {
        eprintln!("Database setup failed: {e}");
        return 1;
    }
    let mut out = std::io::stdout().lock();
    let mut batch = rusqlite::Batch::new(&conn, src);
    loop {
        let mut stmt = match batch.next() {
            Ok(Some(s)) => s,
            Ok(None) => break,
            Err(e) => {
                eprintln!("SQL error: {e}");
                return 1;
            }
        };
        let cols = stmt.column_count();
        if cols == 0 {
            if let Err(e) = stmt.raw_execute() {
                eprintln!("SQL error: {e}");
                return 1;
            }
            continue;
        }
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let _ = writeln!(out, "{}", names.join(" | "));
        let mut rows = stmt.raw_query();
        loop {
            match rows.next() {
                Ok(Some(row)) => {
                    let vals: Vec<String> = (0..cols)
                        .map(|i| match row.get_ref(i) {
                            Ok(rusqlite::types::ValueRef::Null) => "NULL".into(),
                            Ok(rusqlite::types::ValueRef::Integer(v)) => v.to_string(),
                            Ok(rusqlite::types::ValueRef::Real(v)) => v.to_string(),
                            Ok(rusqlite::types::ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
                            Ok(rusqlite::types::ValueRef::Blob(b)) => format!("<{} bytes>", b.len()),
                            Err(e) => format!("<{e}>"),
                        })
                        .collect();
                    let _ = writeln!(out, "{}", vals.join(" | "));
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("SQL error: {e}");
                    return 1;
                }
            }
        }
    }
    let _ = out.flush();
    0
}

fn run_js(src: &str) -> i32 {
    use boa_engine::{js_string, native_function::NativeFunction, object::ObjectInitializer, property::Attribute, Context, JsResult, JsValue, Source};

    fn join_args(args: &[JsValue], ctx: &mut Context) -> JsResult<String> {
        let mut parts = Vec::with_capacity(args.len());
        for a in args {
            let s = if a.is_object() && !a.is_callable() {
                // JSON for arrays/objects, like a browser console's compact view.
                let json = ctx.global_object().get(js_string!("JSON"), ctx)?;
                let stringify = json.as_object().map(|o| o.get(js_string!("stringify"), ctx)).transpose()?;
                match stringify.as_ref().and_then(|s| s.as_callable()) {
                    Some(f) => f.call(&json, &[a.clone()], ctx)?.to_string(ctx)?.to_std_string_escaped(),
                    None => a.to_string(ctx)?.to_std_string_escaped(),
                }
            } else {
                a.to_string(ctx)?.to_std_string_escaped()
            };
            parts.push(s);
        }
        Ok(parts.join(" "))
    }

    fn log(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        let line = join_args(args, ctx)?;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        Ok(JsValue::undefined())
    }

    fn err(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        let line = join_args(args, ctx)?;
        eprintln!("{line}");
        Ok(JsValue::undefined())
    }

    fn readline(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => Ok(JsValue::null()),
            Ok(_) => Ok(JsValue::from(js_string!(line.trim_end_matches(['\r', '\n'])))),
        }
    }

    let mut ctx = Context::default();
    let console = ObjectInitializer::new(&mut ctx)
        .function(NativeFunction::from_fn_ptr(log), js_string!("log"), 0)
        .function(NativeFunction::from_fn_ptr(log), js_string!("info"), 0)
        .function(NativeFunction::from_fn_ptr(err), js_string!("error"), 0)
        .function(NativeFunction::from_fn_ptr(err), js_string!("warn"), 0)
        .build();
    let _ = ctx.register_global_property(js_string!("console"), console, Attribute::all());
    for (name, f) in [("print", log as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>), ("readline", readline), ("prompt", readline)] {
        let _ = ctx.register_global_callable(js_string!(name), 0, NativeFunction::from_fn_ptr(f));
    }
    let result = ctx.eval(Source::from_bytes(src));
    let _ = ctx.run_jobs();
    let _ = std::io::stdout().flush();
    match result {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("Uncaught {e}");
            1
        }
    }
}
