//! Debug mode against the scripted fake adapter.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_term::{application::Application, debug};
use vortex_view::{current_ref, debug::Status};

use super::{
    fake_adapter::{FakeAdapter, Script, Stop},
    harness::Harness,
};
use crate::test::helpers::AppBuilder;

struct Env {
    h: Harness,
    fake: FakeAdapter,
    path: PathBuf,
    dir: PathBuf,
    _tmp: TempDir,
}

async fn env(script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().canonicalize()?;
    let path = dir.join("main.rs");
    let source: String = (1..=30).map(|i| format!("    step({i});\n")).collect();
    std::fs::write(&path, source)?;
    let h = Harness::new(AppBuilder::new().with_file(&path, None))?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().expect("the file has a path").to_owned()
    };
    let fake = FakeAdapter::start(script(&path.to_string_lossy())).await?;
    Ok(Env {
        h,
        fake,
        path,
        dir,
        _tmp: tmp,
    })
}

fn adapter_config(dir: &Path) -> DebugAdapterConfig {
    DebugAdapterConfig {
        name: "fake".into(),
        transport: "tcp".into(),
        command: String::new(),
        args: Vec::new(),
        port_arg: None,
        templates: Vec::new(),
        quirks: Default::default(),
        own_paths: Some(vec![dir.to_owned()]),
        std_paths: None,
        lib_paths: None,
        capabilities: None,
    }
}

impl Env {
    fn launch(&mut self) -> anyhow::Result<()> {
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(&self.dir),
            "fake",
            "launch",
            json!({"program": self.dir.join("prog"), "cwd": self.dir}),
        )
    }
    async fn settled(&mut self, stops: usize) {
        self.h
            .until(
                &format!("{stops} recorded stops and no step running"),
                |app| {
                    let spectator = &app.editor.debug.spectator;
                    spectator.history.stops.len() >= stops
                        && spectator.step.is_none()
                        && matches!(app.editor.debug.status, Status::Stopped { .. })
                },
            )
            .await;
    }
}

fn run(p: &str, n: u64) -> Script {
    (0..n).fold(Script::new(p).lines(), |script, i| {
        script.stop(Stop::new(2 + i, "step"))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explore() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.launch()?;
    e.settled(1).await;
    e.h.keys("<space>G").await?;
    e.h.keys("<space>").await?;
    e.h.pump_for(Duration::from_millis(300)).await;
    let rows = e.h.rows();
    println!("{} x {}", rows.len(), rows[0].chars().count());
    for (i, r) in rows.iter().enumerate() {
        if !r.trim().is_empty() {
            println!("{i:3}|{}", r.trim_end());
        }
    }
    e.h.finish().await
}
