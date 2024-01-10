use redscript::bundle::{ConstantPool, ScriptBundle};
use redscript_compiler::diagnostics::Diagnostic;
use redscript_compiler::source_map::Files;
use redscript_compiler::unit::CompilationUnit;
use redscript_vm::{args, VM};
use serde::Deserialize;
use serenity::all::UserId;
use serenity::framework::standard::macros::help;
use serenity::framework::standard::{
    help_commands, Args, CommandGroup, CommandResult, Configuration, HelpOptions, StandardFramework,
};
use serenity::model::channel::Message;
use serenity::prelude::*;
use serenity::{
    async_trait,
    framework::standard::macros::{command, group},
    prelude::{Context, EventHandler},
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::{fs, io};

#[derive(Deserialize, Debug)]
struct Config {
    cache_file: PathBuf,
    discord_token: String,
}

#[group]
#[commands(eval, run, compile)]
struct General;

struct Handler;

#[async_trait]
impl EventHandler for Handler {}

struct ConstantPoolKey;

impl TypeMapKey for ConstantPoolKey {
    type Value = Arc<ConstantPool>;
}

#[tokio::main]
async fn main() {
    let config = envy::from_env::<Config>().expect("should have a config");
    let mut file =
        io::BufReader::new(fs::File::open(config.cache_file).expect("should open the cache file"));
    let bundle = ScriptBundle::load(&mut file).expect("should load the bundle");

    let framework = StandardFramework::new().group(&GENERAL_GROUP).help(&HELP);
    framework.configure(Configuration::new().prefix("~"));
    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;
    let mut client = Client::builder(config.discord_token, intents)
        .event_handler(Handler)
        .framework(framework)
        .await
        .expect("should create Discord client");

    {
        let mut data = client.data.write().await;
        data.insert::<ConstantPoolKey>(Arc::new(bundle.pool));
    }
    if let Err(why) = client.start().await {
        println!("An error occurred while running the client: {:?}", why);
    }
}

#[help]
async fn help(
    context: &Context,
    msg: &Message,
    args: Args,
    help_options: &'static HelpOptions,
    groups: &[&'static CommandGroup],
    owners: HashSet<UserId>,
) -> CommandResult {
    let _unused =
        help_commands::with_embeds(context, msg, args, help_options, groups, owners).await;
    Ok(())
}

#[command]
#[description = "Evaluates the redscript code and displays the result"]
#[example = " ``1 + 1`` "]
async fn eval(ctx: &Context, msg: &Message) -> CommandResult {
    let body = utils::extract_message_code(msg, "eval");

    let mut sources = Files::new();
    sources.add(
        "code.reds".into(),
        format!(
            " 
        func Main() -> Variant {{
            return {};
        }}    
    ",
            body
        ),
    );
    let pool = ctx
        .data
        .read()
        .await
        .get::<ConstantPoolKey>()
        .expect("should have a constant pool")
        .as_ref()
        .clone();

    match utils::compile_and_execute(pool, &sources) {
        Ok(message) if message.len() >= 2000 => msg.reply(&ctx.http, "Output too large").await?,
        Ok(message) => msg.reply(&ctx.http, message).await?,
        Err(err) => {
            msg.reply(&ctx.http, format!("Could not execute the code: {}", err))
                .await?
        }
    };
    Ok(())
}

#[command]
#[description = "Compiles and runs the redscript code"]
#[example = r#" ``func Main() { FTLog("Hello"); }`` "#]
async fn run(ctx: &Context, msg: &Message) -> CommandResult {
    let body = utils::extract_message_code(msg, "run");

    let mut sources = Files::new();
    sources.add("code.reds".into(), body.to_owned());
    let pool = ctx
        .data
        .read()
        .await
        .get::<ConstantPoolKey>()
        .expect("should have a constant pool")
        .as_ref()
        .clone();

    match utils::compile_and_execute(pool, &sources) {
        Ok(message) if message.len() >= 2000 => msg.reply(&ctx.http, "Output too large").await?,
        Ok(message) => msg.reply(&ctx.http, message).await?,
        Err(err) => {
            msg.reply(&ctx.http, format!("Could not execute the code: {}", err))
                .await?
        }
    };
    Ok(())
}

#[command]
#[description = "Compiles the redscript code"]
#[example = r#" ``func Main() { FTLog("Hello"); }`` "#]
async fn compile(ctx: &Context, msg: &Message) -> CommandResult {
    let body = utils::extract_message_code(msg, "compile");

    let mut sources = Files::new();
    sources.add("code.reds".into(), body.to_owned());
    let mut pool = ctx
        .data
        .read()
        .await
        .get::<ConstantPoolKey>()
        .expect("should have a constant pool")
        .as_ref()
        .clone();

    match utils::compile(&mut pool, &sources) {
        Ok(Some(message)) => msg.reply(&ctx.http, message).await?,
        Ok(None) => {
            msg.reply(&ctx.http, "Code compiles successfully".to_string())
                .await?
        }
        Err(err) => {
            msg.reply(&ctx.http, format!("Could not compile the code: {}", err))
                .await?
        }
    };
    Ok(())
}

mod utils {
    use super::*;

    pub fn extract_message_code<'a>(msg: &'a Message, command: &str) -> &'a str {
        msg.content
            .trim()
            .trim_start_matches('~')
            .trim_start_matches(command)
            .trim()
            .trim_start_matches("```swift")
            .trim_matches('`')
    }

    pub fn compile(pool: &mut ConstantPool, sources: &Files) -> anyhow::Result<Option<String>> {
        let result = CompilationUnit::new_with_defaults(pool)?.compile_files(sources)?;

        let mut diag_out = String::new();
        for diag in result.diagnostics() {
            diag.display(sources, &mut diag_out)?;
        }

        if result.diagnostics().iter().any(Diagnostic::is_fatal) {
            return Ok(Some(format!("```log\n{diag_out}```")));
        }
        Ok(None)
    }

    pub fn compile_and_execute(mut pool: ConstantPool, sources: &Files) -> anyhow::Result<String> {
        if let Some(err) = compile(&mut pool, sources)? {
            return Ok(err);
        }
        let mut vm = VM::new(&pool);

        let stdout = Rc::new(RefCell::new(String::new()));
        let stdout_copy = Rc::clone(&stdout);
        redscript_vm::native::register_natives(&mut vm, move |str| {
            writeln!(stdout_copy.borrow_mut(), "{}", str).unwrap();
        });

        let main = vm
            .metadata()
            .get_function("Main;")
            .ok_or_else(|| anyhow::anyhow!("parameterless Main function not found"))?;

        let message = match vm.call_with_callback(main, args!(), |v| v.map(|v| v.to_string(&pool)))
        {
            Ok(Some(res)) => format!("```log\n{}\n{res}```", stdout.borrow()),
            Ok(None) => format!("```log\n{}```", stdout.borrow()),
            Err(err) => {
                writeln!(stdout.borrow_mut(), "Runtime error: {}", err)?;
                format!("```log\n{}```", stdout.borrow())
            }
        };
        Ok(message)
    }
}
