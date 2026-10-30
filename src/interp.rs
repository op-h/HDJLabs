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
