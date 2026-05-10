use dbx_core::cli::{fail, fail_safe, ok, CliEnvelope, CliErrorCode, CliSource};

const DEFAULT_RESULT_LIMIT: u32 = 50;
const MAX_RESULT_LIMIT: u32 = 1000;

pub(crate) async fn run(args: Vec<String>) -> Result<(), CliEnvelope<()>> {
    let output = dispatch(args).await;
    println!("{}", serde_json::to_string_pretty(&output).unwrap());

    if matches!(output, CliEnvelope::Failure { .. }) {
        std::process::exit(1);
    }

    Ok(())
}

pub(crate) async fn dispatch(args: Vec<String>) -> CliEnvelope<serde_json::Value> {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return err,
    };

    match parsed.positionals.as_slice() {
        [cmd, rest @ ..] if cmd == "context" => context(rest).await,
        [cmd, sub, rest @ ..] if cmd == "conn" && sub == "list" => conn_list(rest).await,
        [cmd, sub, name, rest @ ..] if cmd == "conn" && sub == "show" => conn_show(name, rest).await,
        [cmd, sub, rest @ ..] if cmd == "schema" && sub == "snapshot" => schema_snapshot(rest).await,
        [cmd, rest @ ..] if cmd == "safe-query" => safe_query(rest).await,
        [cmd, rest @ ..] if cmd == "handoff" => handoff(rest).await,
        [cmd, rest @ ..] if cmd == "selection" => selection(rest).await,
        [cmd, sub, rest @ ..] if cmd == "result" && sub == "current" => result_current(rest).await,
        _ => fail(CliSource::Headless, CliErrorCode::InternalError, "Unknown command", false),
    }
}

struct ParsedArgs {
    positionals: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlagKind {
    Value,
    Bool,
}

#[derive(Clone, Copy)]
struct FlagSpec {
    kind: FlagKind,
    allow_dash_value: bool,
}

fn parse_args(args: Vec<String>) -> Result<ParsedArgs, CliEnvelope<serde_json::Value>> {
    let mut positionals = Vec::new();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--format" {
            let Some(value) = args.get(index + 1) else {
                return Err(invalid_args(format!("{arg} requires a value")));
            };
            if value != "json" {
                return Err(invalid_args("Only --format json is supported"));
            }
            index += 2;
        } else if arg.starts_with("--") {
            let Some(spec) =
                flag_spec(positionals.first().map(String::as_str), positionals.get(1).map(String::as_str), arg)
            else {
                return Err(invalid_args(format!("Unknown flag: {arg}")));
            };

            if spec.kind == FlagKind::Value {
                let Some(value) = args.get(index + 1) else {
                    return Err(invalid_args(format!("{arg} requires a value")));
                };
                if !spec.allow_dash_value && value.starts_with("--") {
                    return Err(invalid_args(format!("{arg} requires a value")));
                }
                positionals.push(arg.clone());
                positionals.push(value.clone());
                index += 2;
            } else {
                positionals.push(arg.clone());
                index += 1;
            }
        } else {
            positionals.push(arg.clone());
            index += 1;
        }
    }

    Ok(ParsedArgs { positionals })
}

fn flag_spec(command: Option<&str>, subcommand: Option<&str>, flag: &str) -> Option<FlagSpec> {
    let value = FlagKind::Value;
    let boolean = FlagKind::Bool;
    let normal_value = FlagSpec { kind: value, allow_dash_value: false };
    let free_text_value = FlagSpec { kind: value, allow_dash_value: true };
    let boolean_flag = FlagSpec { kind: boolean, allow_dash_value: false };

    match (command, subcommand, flag) {
        (Some("conn"), Some("show"), "--redacted") => Some(boolean_flag),
        (Some("schema"), Some("snapshot"), "--conn" | "--db") => Some(normal_value),
        (Some("safe-query"), _, "--conn" | "--db" | "--limit") => Some(normal_value),
        (Some("safe-query"), _, "--sql") => Some(free_text_value),
        (Some("handoff"), _, "--conn" | "--sql-file") => Some(normal_value),
        (Some("handoff"), _, "--title" | "--sql" | "--description") => Some(free_text_value),
        (Some("result"), Some("current"), "--limit") => Some(normal_value),
        _ => None,
    }
}

fn reject_unexpected_positionals(
    args: &[String],
    command: &str,
    subcommand: Option<&str>,
) -> Result<(), CliEnvelope<serde_json::Value>> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let Some(spec) = flag_spec(Some(command), subcommand, arg) else {
            return Err(invalid_args(format!("Unexpected positional argument: {arg}")));
        };

        index += match spec.kind {
            FlagKind::Value => 2,
            FlagKind::Bool => 1,
        };
    }
    Ok(())
}

async fn open_state() -> Result<dbx_core::connection::AppState, String> {
    let app_dir = crate::runtime_client::app_data_dir();
    std::fs::create_dir_all(&app_dir).map_err(|err| err.to_string())?;
    let storage = dbx_core::storage::Storage::open(&app_dir.join("dbx.db")).await?;
    Ok(dbx_core::connection::AppState::new(storage))
}

fn redacted_config(config: &dbx_core::models::connection::ConnectionConfig) -> serde_json::Value {
    let risk = dbx_core::sql_safety::risk_for_connection("SELECT 1", &config.name, config.color.as_deref());
    serde_json::json!({
        "id": config.id,
        "name": config.name,
        "databaseType": config.db_type,
        "driverProfile": config.driver_profile,
        "driverLabel": config.driver_label,
        "defaultDatabase": config.database,
        "color": config.color,
        "sshEnabled": config.ssh_enabled,
        "redactedUrl": redacted_url(config),
        "risk": risk,
    })
}

fn redacted_url(config: &dbx_core::models::connection::ConnectionConfig) -> String {
    if matches!(
        config.db_type,
        dbx_core::models::connection::DatabaseType::Sqlite | dbx_core::models::connection::DatabaseType::DuckDb
    ) {
        return redact_embedded_path_url(&config.redacted_connection_url());
    }

    redact_uri_credentials(&config.redacted_connection_url())
}

fn redact_embedded_path_url(value: &str) -> String {
    let suffix_start = value.find(['?', '#']).unwrap_or(value.len());
    format!("<redacted-path>{}", &value[suffix_start..])
}

fn redact_uri_credentials(value: &str) -> String {
    let Some(scheme_index) = value.find("://") else {
        return value.to_string();
    };
    let authority_start = scheme_index + 3;
    let rest = &value[authority_start..];
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_len];
    let Some(at_index) = authority.rfind('@') else {
        return value.to_string();
    };

    format!("{}{}{}", &value[..authority_start], &authority[at_index + 1..], &rest[authority_len..])
}

async fn load_headless_connections(
) -> Result<Vec<dbx_core::models::connection::ConnectionConfig>, CliEnvelope<serde_json::Value>> {
    let state =
        open_state().await.map_err(|err| fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false))?;
    state
        .storage
        .load_connections()
        .await
        .map_err(|err| fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false))
}

async fn find_connection(
    name: &str,
) -> Result<dbx_core::models::connection::ConnectionConfig, CliEnvelope<serde_json::Value>> {
    let configs = load_headless_connections().await?;
    let id_matches: Vec<_> = configs.iter().filter(|config| config.id == name).collect();
    match id_matches.len() {
        1 => return Ok(id_matches[0].clone()),
        0 => {}
        _ => {
            return Err(fail(
                CliSource::Headless,
                CliErrorCode::AmbiguousConnection,
                "Connection id is ambiguous",
                true,
            ));
        }
    }

    let matches: Vec<_> = configs.into_iter().filter(|config| config.name == name).collect();

    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => Err(fail(CliSource::Headless, CliErrorCode::ConnectionNotFound, "Connection not found", true)),
        _ => Err(fail(CliSource::Headless, CliErrorCode::AmbiguousConnection, "Connection name is ambiguous", true)),
    }
}

async fn state_with_connection(
    config: dbx_core::models::connection::ConnectionConfig,
) -> Result<dbx_core::connection::AppState, String> {
    let state = open_state().await?;
    state.configs.lock().await.insert(config.id.clone(), config.clone());

    match config.db_type {
        dbx_core::models::connection::DatabaseType::Sqlite => {
            let path = dbx_core::connection::expand_tilde(&config.host);
            let pool = dbx_core::db::sqlite::connect_path(&path).await?;
            state.connections.lock().await.insert(config.id.clone(), dbx_core::connection::PoolKind::Sqlite(pool));
        }
        dbx_core::models::connection::DatabaseType::DuckDb => {
            let path = dbx_core::connection::expand_tilde(&config.host);
            let pool = dbx_core::db::duckdb_driver::connect_path(&path)?;
            state.connections.lock().await.insert(config.id.clone(), dbx_core::connection::PoolKind::DuckDb(pool));
        }
        _ => {
            state.get_or_create_pool(&config.id, config.database.as_deref()).await?;
        }
    }

    Ok(state)
}

async fn context(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "context", None) {
        return err;
    }

    match crate::runtime_client::get_json("/context").await {
        Ok(data) => ok(CliSource::GuiRuntime, data),
        Err(_) => {
            let configs = match load_headless_connections().await {
                Ok(configs) => configs,
                Err(err) => return err,
            };
            ok(
                CliSource::Headless,
                serde_json::json!({
                    "runtime": "headless",
                    "activeConnection": configs.first().map(redacted_config),
                    "configSource": "headless",
                }),
            )
        }
    }
}

async fn conn_list(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "conn", Some("list")) {
        return err;
    }

    match load_headless_connections().await {
        Ok(configs) => ok(
            CliSource::Headless,
            serde_json::json!({
                "connections": configs.iter().map(redacted_config).collect::<Vec<_>>(),
            }),
        ),
        Err(err) => err,
    }
}

async fn conn_show(name: &str, args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "conn", Some("show")) {
        return err;
    }

    match find_connection(name).await {
        Ok(config) => ok(CliSource::Headless, redacted_config(&config)),
        Err(err) => err,
    }
}

async fn schema_snapshot(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "schema", Some("snapshot")) {
        return err;
    }

    let Some(conn) = option_value(args, "--conn") else {
        return fail(CliSource::Headless, CliErrorCode::ConnectionNotFound, "--conn is required", true);
    };
    let config = match find_connection(conn).await {
        Ok(config) => config,
        Err(err) => return err,
    };
    let state = match state_with_connection(config.clone()).await {
        Ok(state) => state,
        Err(err) => return fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false),
    };

    match dbx_core::schema_snapshot::snapshot(&state, &config.id, option_value(args, "--db"), None).await {
        Ok(snapshot) => ok(CliSource::Headless, serde_json::to_value(snapshot).unwrap()),
        Err(err) => fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false),
    }
}

async fn safe_query(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "safe-query", None) {
        return err;
    }

    let Some(conn) = option_value(args, "--conn") else {
        return fail(CliSource::Headless, CliErrorCode::ConnectionNotFound, "--conn is required", true);
    };
    let Some(sql) = option_value(args, "--sql") else {
        return fail(CliSource::Headless, CliErrorCode::QueryClassificationFailed, "--sql is required", true);
    };
    let limit = match parse_result_limit(args) {
        Ok(limit) => limit as usize,
        Err(err) => return err,
    };
    let config = match find_connection(conn).await {
        Ok(config) => config,
        Err(err) => return err,
    };
    let risk = dbx_core::sql_safety::risk_for_connection(sql, &config.name, config.color.as_deref());
    match risk.operation_class {
        dbx_core::sql_safety::OperationClass::Read => {}
        dbx_core::sql_safety::OperationClass::Write if risk.is_production => {
            return blocked_query(CliErrorCode::ProductionWriteBlocked, &risk);
        }
        dbx_core::sql_safety::OperationClass::Write => {
            return blocked_query(CliErrorCode::HandoffRequired, &risk);
        }
        dbx_core::sql_safety::OperationClass::Ddl => {
            return blocked_query(CliErrorCode::DdlBlocked, &risk);
        }
        dbx_core::sql_safety::OperationClass::Unknown => {
            return blocked_query(CliErrorCode::QueryClassificationFailed, &risk);
        }
    }
    let state = match state_with_connection(config.clone()).await {
        Ok(state) => state,
        Err(err) => return fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false),
    };
    let database = option_value(args, "--db").or(config.database.as_deref()).unwrap_or("");

    match dbx_core::query::execute_sql_statement_with_row_limit(&state, &config.id, database, sql, None, None, limit)
        .await
    {
        Ok(result) => ok(
            CliSource::Headless,
            serde_json::json!({
                "risk": risk,
                "result": result,
            }),
        ),
        Err(err) => fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false),
    }
}

fn blocked_query(code: CliErrorCode, risk: &dbx_core::sql_safety::RiskMetadata) -> CliEnvelope<serde_json::Value> {
    fail(CliSource::Headless, code, serde_json::to_string(risk).unwrap(), true)
}

async fn handoff(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "handoff", None) {
        return err;
    }

    let Some(conn) = required_option(args, "--conn") else {
        return fail(CliSource::Headless, CliErrorCode::ConnectionNotFound, "--conn is required", true);
    };
    let Some(title) = required_option(args, "--title") else {
        return invalid_args("--title is required");
    };
    let sql_inline = option_value(args, "--sql");
    let sql_file = option_value(args, "--sql-file");
    let sql = match (sql_inline, sql_file) {
        (Some(_), Some(_)) => return invalid_args("Use exactly one of --sql or --sql-file"),
        (Some(sql), None) => sql.to_string(),
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(sql) => sql,
            Err(err) => return invalid_args(format!("Failed to read --sql-file: {err}")),
        },
        (None, None) => return invalid_args("Use exactly one of --sql or --sql-file"),
    };

    if sql.trim().is_empty() {
        return invalid_args("SQL must not be empty");
    }

    let config = match find_connection(conn).await {
        Ok(config) => config,
        Err(err) => return err,
    };
    let risk = dbx_core::sql_safety::risk_for_connection(&sql, &config.name, config.color.as_deref());
    let item = dbx_core::handoff::HandoffItem::queued(
        config.id,
        config.name,
        config.database,
        title.to_string(),
        option_value(args, "--description").map(str::to_string),
        sql,
        risk.operation_class,
        risk.risk_level,
        risk.is_production,
    );

    if let Ok(data) = crate::runtime_client::post_json("/handoff", serde_json::to_value(&item).unwrap()).await {
        return ok(CliSource::GuiRuntime, data);
    }

    match queue_handoff(&item).await {
        Ok(()) => ok(CliSource::Headless, serde_json::json!({ "id": item.id, "status": "queued" })),
        Err(err) => fail_safe(CliSource::Headless, CliErrorCode::InternalError, err, false),
    }
}

async fn selection(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "selection", None) {
        return err;
    }

    match crate::runtime_client::get_json("/selection").await {
        Ok(data) => ok(CliSource::GuiRuntime, data),
        Err(_) => runtime_required("dbx selection requires DBX GUI runtime."),
    }
}

async fn result_current(args: &[String]) -> CliEnvelope<serde_json::Value> {
    if let Err(err) = reject_unexpected_positionals(args, "result", Some("current")) {
        return err;
    }

    let limit = match parse_result_limit(args) {
        Ok(limit) => limit,
        Err(err) => return err,
    };

    match crate::runtime_client::get_json_with_query("/result/current", &[("limit", limit.to_string())]).await {
        Ok(data) => ok(CliSource::GuiRuntime, data),
        Err(_) => runtime_required("dbx result current requires DBX GUI runtime."),
    }
}

fn runtime_required(message: &str) -> CliEnvelope<serde_json::Value> {
    fail(CliSource::Headless, CliErrorCode::GuiRuntimeRequired, message, true)
}

fn invalid_args(message: impl Into<String>) -> CliEnvelope<serde_json::Value> {
    fail(CliSource::Headless, CliErrorCode::InternalError, message, true)
}

fn option_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|pair| pair[0] == key).map(|pair| pair[1].as_str())
}

fn required_option<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    option_value(args, key).map(str::trim).filter(|value| !value.is_empty())
}

fn parse_result_limit(args: &[String]) -> Result<u32, CliEnvelope<serde_json::Value>> {
    let default_limit = DEFAULT_RESULT_LIMIT.to_string();
    let raw = option_value(args, "--limit").unwrap_or(&default_limit);
    let limit = raw
        .parse::<u32>()
        .map_err(|_| invalid_args(format!("--limit must be a positive integer between 1 and {MAX_RESULT_LIMIT}")))?;
    if !(1..=MAX_RESULT_LIMIT).contains(&limit) {
        return Err(invalid_args(format!("--limit must be a positive integer between 1 and {MAX_RESULT_LIMIT}")));
    }
    Ok(limit)
}

async fn queue_handoff(item: &dbx_core::handoff::HandoffItem) -> Result<(), String> {
    let app_dir = crate::runtime_client::app_data_dir();
    std::fs::create_dir_all(&app_dir).map_err(|err| err.to_string())?;
    let storage = dbx_core::storage::Storage::open(&app_dir.join("dbx.db")).await?;
    storage.save_handoff(item).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_client::ENV_LOCK;
    use dbx_core::cli::CliErrorCode;
    use dbx_core::models::connection::{ConnectionConfig, DatabaseType};

    fn assert_failure_code(env: CliEnvelope<serde_json::Value>, expected: CliErrorCode) {
        match env {
            CliEnvelope::Failure { error, .. } => assert_eq!(error.code, expected),
            CliEnvelope::Success { .. } => panic!("expected failure envelope"),
        }
    }

    fn assert_failure_message_contains(env: CliEnvelope<serde_json::Value>, expected: &str) {
        match env {
            CliEnvelope::Failure { error, .. } => assert!(
                error.message.contains(expected),
                "expected error message to contain {expected:?}, got {:?}",
                error.message
            ),
            CliEnvelope::Success { .. } => panic!("expected failure envelope"),
        }
    }

    fn mysql_fixture(id: &str, name: &str, color: Option<&str>, password: &str) -> ConnectionConfig {
        ConnectionConfig {
            id: id.to_string(),
            name: name.to_string(),
            db_type: DatabaseType::Mysql,
            driver_profile: None,
            driver_label: Some("MySQL".to_string()),
            url_params: None,
            host: "127.0.0.1".to_string(),
            port: 3306,
            username: "root".to_string(),
            password: password.to_string(),
            database: Some("app".to_string()),
            color: color.map(str::to_string),
            ssh_enabled: true,
            ssh_host: "bastion.internal".to_string(),
            ssh_port: 22,
            ssh_user: "deploy".to_string(),
            ssh_password: "ssh-secret".to_string(),
            ssh_key_path: String::new(),
            ssh_key_passphrase: "key-secret".to_string(),
            ssh_expose_lan: false,
            ssh_connect_timeout_secs: dbx_core::models::connection::default_ssh_connect_timeout_secs(),
            ssl: false,
            sysdba: false,
            connection_string: Some(format!("mysql://root:{password}@127.0.0.1:3306/app")),
            jdbc_driver_class: None,
            jdbc_driver_paths: Vec::new(),
        }
    }

    fn embedded_fixture(id: &str, name: &str, db_type: DatabaseType, path: &std::path::Path) -> ConnectionConfig {
        let mut config = mysql_fixture(id, name, None, "");
        config.db_type = db_type;
        config.host = path.display().to_string();
        config.port = 0;
        config.username = String::new();
        config.password = String::new();
        config.database = None;
        config.connection_string = None;
        config
    }

    async fn seed_connections(dir: &std::path::Path, configs: &[ConnectionConfig]) {
        std::fs::create_dir_all(dir).unwrap();
        let storage = dbx_core::storage::Storage::open(&dir.join("dbx.db")).await.unwrap();
        storage.save_connections(configs).await.unwrap();
    }

    async fn create_sqlite_fixture(path: &std::path::Path) {
        std::fs::File::create(path).unwrap();
        let pool = dbx_core::db::sqlite::connect_path(&path.display().to_string()).await.unwrap();
        dbx_core::db::sqlite::execute_query(&pool, "CREATE TABLE teams (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .await
            .unwrap();
        dbx_core::db::sqlite::execute_query(
            &pool,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, team_id INTEGER NOT NULL, email TEXT NOT NULL, FOREIGN KEY(team_id) REFERENCES teams(id))",
        )
        .await
        .unwrap();
        dbx_core::db::sqlite::execute_query(&pool, "INSERT INTO teams (id, name) VALUES (1, 'Core'), (2, 'Data')")
            .await
            .unwrap();
        dbx_core::db::sqlite::execute_query(
            &pool,
            "INSERT INTO users (id, team_id, email) VALUES (1, 1, 'ada@example.com'), (2, 2, 'grace@example.com')",
        )
        .await
        .unwrap();
        pool.close().await;
    }

    async fn create_large_sqlite_fixture(path: &std::path::Path, row_count: usize) {
        std::fs::File::create(path).unwrap();
        let pool = dbx_core::db::sqlite::connect_path(&path.display().to_string()).await.unwrap();
        dbx_core::db::sqlite::execute_query(
            &pool,
            "CREATE TABLE numbers (id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
        )
        .await
        .unwrap();
        for id in 1..=row_count {
            dbx_core::db::sqlite::execute_query(
                &pool,
                &format!("INSERT INTO numbers (id, value) VALUES ({id}, 'value-{id}')"),
            )
            .await
            .unwrap();
        }
        pool.close().await;
    }

    fn success_data(env: CliEnvelope<serde_json::Value>) -> serde_json::Value {
        match env {
            CliEnvelope::Success { source, data, .. } => {
                assert_eq!(source, CliSource::Headless);
                data
            }
            CliEnvelope::Failure { error, .. } => panic!("expected success envelope, got {error:?}"),
        }
    }

    #[test]
    fn parser_allows_free_text_option_values_to_start_with_dashes() {
        let parsed = parse_args(vec![
            "handoff".into(),
            "--conn".into(),
            "local".into(),
            "--title".into(),
            "-- review generated SQL".into(),
            "--sql".into(),
            "-- explain select 1".into(),
            "--description".into(),
            "-- optional note".into(),
        ])
        .expect("free-text values beginning with -- should parse");

        assert_eq!(
            parsed.positionals,
            vec![
                "handoff",
                "--conn",
                "local",
                "--title",
                "-- review generated SQL",
                "--sql",
                "-- explain select 1",
                "--description",
                "-- optional note",
            ]
        );
    }

    #[test]
    fn parser_accepts_global_json_format_before_between_and_after_command_args() {
        let cases = [
            vec!["--format", "json", "safe-query", "--conn", "local", "--sql", "select 1"],
            vec!["safe-query", "--conn", "local", "--format", "json", "--sql", "select 1"],
            vec!["safe-query", "--conn", "local", "--sql", "select 1", "--format", "json"],
        ];

        for args in cases {
            let parsed = parse_args(args.iter().map(|value| value.to_string()).collect())
                .unwrap_or_else(|err| panic!("expected parse success for {args:?}, got {err:?}"));
            assert_eq!(parsed.positionals, vec!["safe-query", "--conn", "local", "--sql", "select 1"]);
        }
    }

    #[tokio::test]
    async fn rejects_extra_positionals_for_each_command() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());

        let cases = [
            vec!["context", "extra"],
            vec!["conn", "list", "extra"],
            vec!["conn", "show", "local", "extra"],
            vec!["schema", "snapshot", "--conn", "local", "extra"],
            vec!["safe-query", "--conn", "local", "--sql", "select 1", "extra"],
            vec!["handoff", "--conn", "local", "--title", "Review", "--sql", "select 1", "extra"],
            vec!["selection", "extra"],
            vec!["result", "current", "--limit", "50", "extra"],
        ];

        for args in cases {
            let env = dispatch(args.iter().map(|value| value.to_string()).collect()).await;
            assert_failure_message_contains(env, "Unexpected positional argument");
        }

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn gui_only_commands_return_runtime_required_without_runtime() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());

        assert_failure_code(
            dispatch(vec!["selection".into(), "--format".into(), "json".into()]).await,
            CliErrorCode::GuiRuntimeRequired,
        );
        assert_failure_code(
            dispatch(vec![
                "result".into(),
                "current".into(),
                "--limit".into(),
                "25".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
            CliErrorCode::GuiRuntimeRequired,
        );

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn recognizes_all_eight_cli_commands_with_json_format() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());

        let cases = [
            vec!["context", "--format", "json"],
            vec!["conn", "list", "--format", "json"],
            vec!["conn", "show", "__missing__", "--redacted", "--format", "json"],
            vec!["schema", "snapshot", "--format", "json"],
            vec!["safe-query", "--format", "json"],
            vec!["handoff", "--format", "json"],
            vec!["selection", "--format", "json"],
            vec!["result", "current", "--limit", "50", "--format", "json"],
        ];

        for args in cases {
            let env = dispatch(args.iter().map(|value| value.to_string()).collect()).await;
            let json = serde_json::to_value(&env).unwrap();
            assert!(json.get("ok").is_some(), "missing ok for args: {args:?}");
            assert!(json.get("source").is_some(), "missing source for args: {args:?}");
            assert!(json.get("data").is_some() || json.get("error").is_some(), "missing data/error for args: {args:?}");
        }

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn context_reads_headless_storage_and_returns_redacted_active_connection() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(dir.path(), &[mysql_fixture("prod-id", "prod-main", Some("#ef4444"), "super-secret")]).await;

        let data = success_data(dispatch(vec!["context".into(), "--format".into(), "json".into()]).await);

        assert_eq!(data["runtime"], "headless");
        assert_eq!(data["activeConnection"]["id"], "prod-id");
        assert_eq!(data["activeConnection"]["name"], "prod-main");
        assert_eq!(data["activeConnection"]["risk"]["isProduction"], true);
        assert_eq!(data["configSource"], "headless");
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("configSource"));
        assert!(!json.contains(&dir.path().join("dbx.db").display().to_string()));
        assert!(!json.contains("super-secret"));
        assert!(!json.contains("ssh-secret"));
        assert!(!json.contains("key-secret"));

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn conn_list_reads_storage_and_returns_redacted_connection_dtos_with_risk_metadata() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[
                mysql_fixture("prod-id", "prod-main", Some("#ef4444"), "prod-secret"),
                mysql_fixture("dev-id", "dev-main", Some("#22c55e"), "dev-secret"),
            ],
        )
        .await;

        let data = success_data(dispatch(vec!["conn".into(), "list".into(), "--format".into(), "json".into()]).await);
        let connections = data["connections"].as_array().expect("connections should be an array");

        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0]["id"], "prod-id");
        assert_eq!(connections[0]["redactedUrl"], "mysql://127.0.0.1:3306/app?ssl-mode=preferred&charset=utf8mb4");
        assert_eq!(connections[0]["risk"]["isProduction"], true);
        assert_eq!(connections[0]["risk"]["productionReason"], "red connection color");
        assert_eq!(connections[1]["risk"]["isProduction"], false);
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("prod-secret"));
        assert!(!json.contains("dev-secret"));
        assert!(!json.contains("root:"));

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn conn_list_redacts_sqlite_and_duckdb_file_paths() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        let sqlite_path = dir.path().join("private").join("app.sqlite");
        let duckdb_path = dir.path().join("private").join("warehouse.duckdb");
        seed_connections(
            dir.path(),
            &[
                embedded_fixture("sqlite-id", "local-sqlite", DatabaseType::Sqlite, &sqlite_path),
                embedded_fixture("duckdb-id", "local-duckdb", DatabaseType::DuckDb, &duckdb_path),
            ],
        )
        .await;

        let data = success_data(dispatch(vec!["conn".into(), "list".into(), "--format".into(), "json".into()]).await);
        let connections = data["connections"].as_array().expect("connections should be an array");

        assert_eq!(connections[0]["redactedUrl"], "<redacted-path>?mode=rwc");
        assert_eq!(connections[1]["redactedUrl"], "<redacted-path>?mode=rwc");
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("app.sqlite"));
        assert!(!json.contains("warehouse.duckdb"));
        assert!(!json.contains(dir.path().to_str().unwrap()));

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn conn_show_supports_id_lookup_and_returns_not_found_or_ambiguous_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[
                mysql_fixture("prod-id", "prod-main", Some("#ef4444"), "prod-secret"),
                mysql_fixture("dup-1", "shared", None, "first-secret"),
                mysql_fixture("dup-2", "shared", None, "second-secret"),
            ],
        )
        .await;

        let shown = success_data(
            dispatch(vec![
                "conn".into(),
                "show".into(),
                "prod-id".into(),
                "--redacted".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
        );
        assert_eq!(shown["id"], "prod-id");
        assert_eq!(shown["risk"]["isProduction"], true);
        assert!(!serde_json::to_string(&shown).unwrap().contains("prod-secret"));

        assert_failure_code(
            dispatch(vec!["conn".into(), "show".into(), "__missing__".into(), "--format".into(), "json".into()]).await,
            CliErrorCode::ConnectionNotFound,
        );
        assert_failure_code(
            dispatch(vec!["conn".into(), "show".into(), "shared".into(), "--format".into(), "json".into()]).await,
            CliErrorCode::AmbiguousConnection,
        );

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn conn_show_unique_id_match_takes_precedence_over_ambiguous_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[
                mysql_fixture("shared", "id-target", Some("#22c55e"), "target-secret"),
                mysql_fixture("dup-1", "shared", None, "first-secret"),
                mysql_fixture("dup-2", "shared", None, "second-secret"),
            ],
        )
        .await;

        let shown = success_data(
            dispatch(vec!["conn".into(), "show".into(), "shared".into(), "--format".into(), "json".into()]).await,
        );

        assert_eq!(shown["id"], "shared");
        assert_eq!(shown["name"], "id-target");
        let json = serde_json::to_string(&shown).unwrap();
        assert!(!json.contains("target-secret"));
        assert!(!json.contains("first-secret"));
        assert!(!json.contains("second-secret"));

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn schema_snapshot_executes_headless_sqlite_snapshot_from_storage() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("fixture.sqlite");
        create_sqlite_fixture(&data_path).await;
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[embedded_fixture("sqlite-id", "local-sqlite", DatabaseType::Sqlite, &data_path)],
        )
        .await;

        let data = success_data(
            dispatch(vec![
                "schema".into(),
                "snapshot".into(),
                "--conn".into(),
                "local-sqlite".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
        );

        assert_eq!(data["connectionId"], "sqlite-id");
        assert_eq!(data["database"], "main");
        let tables = data["tables"].as_array().expect("tables should be an array");
        assert!(tables.iter().any(|table| table["name"] == "users"));

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn safe_query_executes_read_sqlite_query_with_limit_and_risk() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("fixture.sqlite");
        create_sqlite_fixture(&data_path).await;
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[embedded_fixture("sqlite-id", "local-sqlite", DatabaseType::Sqlite, &data_path)],
        )
        .await;

        let data = success_data(
            dispatch(vec![
                "safe-query".into(),
                "--conn".into(),
                "sqlite-id".into(),
                "--sql".into(),
                "SELECT email FROM users ORDER BY id".into(),
                "--limit".into(),
                "1".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
        );

        assert_eq!(data["risk"]["operationClass"], "read");
        assert_eq!(data["result"]["columns"], serde_json::json!(["email"]));
        assert_eq!(data["result"]["rows"], serde_json::json!([["ada@example.com"]]));
        assert_eq!(data["result"]["truncated"], true);

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn safe_query_expands_tilde_for_sqlite_connection_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let data_path = home.join("fixture.sqlite");
        create_sqlite_fixture(&data_path).await;
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        let mut config = embedded_fixture("sqlite-id", "local-sqlite", DatabaseType::Sqlite, &data_path);
        config.host = "~/fixture.sqlite".to_string();
        seed_connections(dir.path(), &[config]).await;

        let data = success_data(
            dispatch(vec![
                "safe-query".into(),
                "--conn".into(),
                "local-sqlite".into(),
                "--sql".into(),
                "SELECT COUNT(*) FROM users".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
        );

        assert_eq!(data["result"]["rows"], serde_json::json!([[2]]));

        std::env::remove_var("DBX_APP_DATA_DIR");
        if let Some(previous_home) = previous_home {
            std::env::set_var("HOME", previous_home);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[tokio::test]
    async fn safe_query_large_sqlite_result_set_respects_limit() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("large.sqlite");
        create_large_sqlite_fixture(&data_path, 200).await;
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[embedded_fixture("sqlite-id", "local-sqlite", DatabaseType::Sqlite, &data_path)],
        )
        .await;

        let data = success_data(
            dispatch(vec![
                "safe-query".into(),
                "--conn".into(),
                "sqlite-id".into(),
                "--sql".into(),
                "SELECT id, value FROM numbers ORDER BY id".into(),
                "--limit".into(),
                "7".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
        );

        let rows = data["result"]["rows"].as_array().expect("rows should be an array");
        assert!(rows.len() <= 7, "safe-query returned {} rows for limit 7", rows.len());
        assert_eq!(rows.len(), 7);
        assert_eq!(data["result"]["truncated"], true);

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn schema_snapshot_and_safe_query_sanitize_internal_error_envelopes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let private_path = dir.path().join("private").join("missing.sqlite");
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(
            dir.path(),
            &[embedded_fixture("sqlite-id", "local-sqlite", DatabaseType::Sqlite, &private_path)],
        )
        .await;

        let cases = [
            dispatch(vec![
                "schema".into(),
                "snapshot".into(),
                "--conn".into(),
                "sqlite-id".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
            dispatch(vec![
                "safe-query".into(),
                "--conn".into(),
                "sqlite-id".into(),
                "--sql".into(),
                "SELECT 1".into(),
                "--format".into(),
                "json".into(),
            ])
            .await,
        ];

        for env in cases {
            match env {
                CliEnvelope::Failure { error, .. } => {
                    let message = error.message;
                    assert!(!message.contains(dir.path().to_str().unwrap()));
                    assert!(!message.contains("missing.sqlite"));
                    assert!(!message.contains("Database file does not exist"));
                    assert!(!message.contains("SQLite connection failed"));
                }
                CliEnvelope::Success { .. } => panic!("expected sanitized failure envelope"),
            }
        }

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn safe_query_blocks_non_read_sql_with_structured_risk_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("fixture.sqlite");
        create_sqlite_fixture(&data_path).await;
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        let mut prod = embedded_fixture("prod-sqlite", "prod-sqlite", DatabaseType::Sqlite, &data_path);
        prod.color = Some("#ef4444".to_string());
        let dev = embedded_fixture("dev-sqlite", "dev-sqlite", DatabaseType::Sqlite, &data_path);
        seed_connections(dir.path(), &[prod, dev]).await;

        let cases = [
            (
                "prod-sqlite",
                "UPDATE users SET email = 'x@example.com' WHERE id = 1",
                CliErrorCode::ProductionWriteBlocked,
                "write",
                true,
            ),
            (
                "dev-sqlite",
                "UPDATE users SET email = 'x@example.com' WHERE id = 1",
                CliErrorCode::HandoffRequired,
                "write",
                false,
            ),
            ("prod-sqlite", "DROP TABLE users", CliErrorCode::DdlBlocked, "ddl", true),
            ("prod-sqlite", "VACUUM", CliErrorCode::QueryClassificationFailed, "unknown", true),
        ];

        for (conn, sql, expected_code, expected_class, expected_production) in cases {
            match dispatch(vec![
                "safe-query".into(),
                "--conn".into(),
                conn.into(),
                "--sql".into(),
                sql.into(),
                "--format".into(),
                "json".into(),
            ])
            .await
            {
                CliEnvelope::Failure { error, .. } => {
                    assert_eq!(error.code, expected_code);
                    let risk: serde_json::Value = serde_json::from_str(&error.message).unwrap();
                    assert_eq!(risk["operationClass"], expected_class);
                    assert_eq!(risk["isProduction"], expected_production);
                }
                CliEnvelope::Success { .. } => panic!("expected safe-query to block {sql}"),
            }
        }

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn unknown_command_returns_internal_error_envelope() {
        assert_failure_code(
            dispatch(vec!["not-a-command".into(), "--format".into(), "json".into()]).await,
            CliErrorCode::InternalError,
        );
    }

    #[tokio::test]
    async fn rejects_non_json_format() {
        assert_failure_code(
            dispatch(vec!["context".into(), "--format".into(), "text".into()]).await,
            CliErrorCode::InternalError,
        );
    }

    #[tokio::test]
    async fn rejects_unknown_flags_with_error_envelope() {
        assert_failure_code(
            dispatch(vec!["context".into(), "--unknown".into(), "value".into()]).await,
            CliErrorCode::InternalError,
        );
    }

    #[tokio::test]
    async fn rejects_missing_option_values_with_error_envelope() {
        assert_failure_code(
            dispatch(vec!["safe-query".into(), "--conn".into(), "--sql".into(), "select 1".into()]).await,
            CliErrorCode::InternalError,
        );
    }

    #[tokio::test]
    async fn validates_handoff_required_options_and_sql_source() {
        assert_failure_code(
            dispatch(vec!["handoff".into(), "--title".into(), "Review".into(), "--sql".into(), "select 1".into()])
                .await,
            CliErrorCode::ConnectionNotFound,
        );
        assert_failure_code(
            dispatch(vec!["handoff".into(), "--conn".into(), "local".into(), "--sql".into(), "select 1".into()]).await,
            CliErrorCode::InternalError,
        );
        assert_failure_code(
            dispatch(vec![
                "handoff".into(),
                "--conn".into(),
                "local".into(),
                "--title".into(),
                "Review".into(),
                "--sql".into(),
                "select 1".into(),
                "--sql-file".into(),
                "query.sql".into(),
            ])
            .await,
            CliErrorCode::InternalError,
        );
    }

    #[tokio::test]
    async fn handoff_queues_sql_file_when_runtime_is_unavailable() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let sql_file = dir.path().join("query.sql");
        std::fs::write(&sql_file, "select 1").unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(dir.path(), &[mysql_fixture("local-id", "local", Some("#22c55e"), "local-secret")]).await;

        let env = dispatch(vec![
            "handoff".into(),
            "--conn".into(),
            "local".into(),
            "--title".into(),
            "Review".into(),
            "--sql-file".into(),
            sql_file.display().to_string(),
        ])
        .await;

        match env {
            CliEnvelope::Success { source, data, .. } => {
                assert_eq!(source, CliSource::Headless);
                assert_eq!(data["status"], "queued");
            }
            CliEnvelope::Failure { error, .. } => panic!("expected queued handoff, got {error:?}"),
        }

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn handoff_queues_with_resolved_connection_metadata_and_risk() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DBX_APP_DATA_DIR", dir.path());
        seed_connections(dir.path(), &[mysql_fixture("prod-id", "prod-main", Some("#ef4444"), "prod-secret")]).await;

        let env = dispatch(vec![
            "handoff".into(),
            "--conn".into(),
            "prod-id".into(),
            "--title".into(),
            "Review write".into(),
            "--description".into(),
            "generated by agent".into(),
            "--sql".into(),
            "UPDATE users SET active = 0 WHERE id = 1".into(),
            "--format".into(),
            "json".into(),
        ])
        .await;

        match env {
            CliEnvelope::Success { source, data, .. } => {
                assert_eq!(source, CliSource::Headless);
                assert_eq!(data["status"], "queued");
            }
            CliEnvelope::Failure { error, .. } => panic!("expected queued handoff, got {error:?}"),
        }

        let storage = dbx_core::storage::Storage::open(&dir.path().join("dbx.db")).await.unwrap();
        let queued = storage.load_pending_handoffs().await.unwrap();

        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].connection_id, "prod-id");
        assert_eq!(queued[0].connection_name, "prod-main");
        assert_eq!(queued[0].database.as_deref(), Some("app"));
        assert_eq!(queued[0].risk_level, dbx_core::sql_safety::RiskLevel::High);
        assert!(queued[0].is_production);

        std::env::remove_var("DBX_APP_DATA_DIR");
    }

    #[tokio::test]
    async fn result_current_rejects_non_positive_and_over_limit_values() {
        for limit in ["0", "-1", "1001", "abc"] {
            assert_failure_code(
                dispatch(vec!["result".into(), "current".into(), "--limit".into(), limit.into()]).await,
                CliErrorCode::InternalError,
            );
        }
    }
}
