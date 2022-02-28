use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crate::{
    core::{
        config::JudgerConfig,
        misc::ResultType,
        state::{AppState, GLOBAL_APP_STATE},
    },
    task::{local::local_judge_task_handler, online_ide::online_ide_handler},
};
use anyhow::anyhow;
use celery::{broker::RedisBrokerBuilder, CeleryBuilder};
use config::Config;
use flexi_logger::{DeferredNow, Record, TS_DASHES_BLANK_COLONS_DOT_BLANK};
use log::info;
pub mod core;
pub mod task;
pub fn my_log_format(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    write!(
        w,
        "[{}] {} [{}:{}] {}",
        now.format(TS_DASHES_BLANK_COLONS_DOT_BLANK),
        record.level(),
        record.module_path().unwrap_or("<unnamed>"),
        record.line().unwrap_or(0),
        &record.args()
    )
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ResultType<()> {
    if !std::path::Path::new("config.yaml").exists() {
        tokio::fs::write(
            "config.yaml",
            serde_yaml::to_string(&JudgerConfig::default())?.as_bytes(),
        )
        .await?;
        return Err(anyhow!(
            "Config not found. Default config file created, modify it and restart this judger."
        ));
    }
    let builder = Config::builder()
        .add_source(Config::try_from(&JudgerConfig::default())?)
        .add_source(Config::try_from(
            &serde_yaml::from_str::<JudgerConfig>(
                &tokio::fs::read_to_string("config.yaml")
                    .await
                    .map_err(|e| anyhow!("Failed to read configure file: {}", e))?,
            )
            .map_err(|e| anyhow!("Failed to deserialize configure file: {}", e))?,
        )?);
    let config: JudgerConfig = builder.build()?.try_deserialize()?;
    use flexi_logger::{Duplicate, FileSpec, Logger};
    Logger::try_with_str(&config.logging_level)
        .map_err(|_| anyhow!("Invalid loggine level: {}", config.logging_level))?
        .format(my_log_format)
        .log_to_file(FileSpec::default().directory("logs").basename("hj3-judger"))
        .duplicate_to_stdout(Duplicate::All)
        .start()
        .map_err(|e| anyhow!("Failed to start logger!\n{}", e))?;
    info!("Hellojudge3 Judger, version {}", env!("CARGO_PKG_VERSION"));
    info!("Logger starting..");
    info!("Loaded config:\n{:#?}", config);
    let data_dir = PathBuf::from(config.data_dir.clone());
    if !data_dir.exists() {
        std::fs::create_dir(&data_dir).expect("Failed to create data dir");
    }
    let app_state = AppState {
        config,
        file_dir_locks: tokio::sync::Mutex::new(HashMap::default()),
        testdata_dir: data_dir,
    };
    *GLOBAL_APP_STATE.write().await = Some(app_state);
    let guard = GLOBAL_APP_STATE.read().await;
    let app_state = guard.as_ref().unwrap();
    let celery_app = Arc::new(
        CeleryBuilder::<RedisBrokerBuilder>::new("hj3-judger", &app_state.config.broker_url)
            .task_retry_for_unexpected(false)
            .prefetch_count(app_state.config.prefetch_count)
            .build()
            .await?,
    );
    celery_app
        .register_task::<local_judge_task_handler>()
        .await
        .expect("Failed to register local judge handler");
    celery_app
        .register_task::<online_ide_handler>()
        .await
        .expect("Failed to register online ide handler");

    info!("Started!");
    celery_app.consume().await.unwrap();
    return Ok(());
}
