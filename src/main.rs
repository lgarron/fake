use async_std::task::{self, block_on, JoinHandle};
use futures::{future::join_all, FutureExt};
use indicatif::{MultiProgress, ProgressBar, ProgressFinish, ProgressStyle};
mod options;
use std::{
    collections::HashMap,
    fs::read_to_string,
    path::{Path, PathBuf},
    process::{exit, Command},
    sync::Arc,
    time::{Duration, Instant},
};

use options::get_options;
use parse::TargetName;

use crate::parse::TargetGraph;

mod parse;

fn main() {
    let start_time = Instant::now();
    let options = get_options();

    let makefile_path = options.makefile_path.unwrap_or("Makefile".into());
    let makefile_contents = read_to_string(&makefile_path).unwrap_or_else(|_| {
        eprintln!("Could not read Makefile");
        exit(1)
    });
    let target_graph: TargetGraph =
        TargetGraph::try_from(&makefile_contents).expect("Could not parse Makefile");

    if options.print_graph {
        println!(
            "{}",
            serde_json::to_string_pretty(&target_graph).expect("Could not print graph")
        );
        exit(0)
    }

    let default_target_name = match target_graph.0.keys().next() {
        Some(target_name) => target_name,
        None => {
            eprintln!("No target specified and no default target available");
            exit(1)
        }
    };
    let main_target_name = match options.target {
        Some(target_name) => {
            let target_name = TargetName(target_name);
            if !target_graph.0.contains_key(&target_name) {
                eprintln!("Unknown target specified: {}", target_name);
                exit(1)
            };
            target_name
        }
        None => default_target_name.clone(),
    };

    let multi_progress = Arc::new(MultiProgress::new());

    let mut shared_make = SharedMake {
        multi_progress: multi_progress.clone(),
        futures: HashMap::default(),
        target_graph,
        makefile_path,
    };

    block_on(shared_make.make_target(&main_target_name, 0));
    println!(
        "Built {} targets in {:?}",
        shared_make.futures.len(),
        Instant::now() - start_time
    );
}

type SharedFuture = futures::future::Shared<JoinHandle<()>>;

struct SharedMake {
    multi_progress: Arc<MultiProgress>,
    futures: HashMap<TargetName, SharedFuture>,
    target_graph: TargetGraph,
    makefile_path: PathBuf,
}

impl SharedMake {
    fn make_target(&mut self, target_name: &TargetName, depth: usize) -> SharedFuture {
        if let Some(sender) = self.futures.get(target_name) {
            return sender.clone();
        }

        let dependencies = self
            .target_graph
            .0
            .get(target_name)
            .expect("Internal error: Unexpectedly missing a target")
            .clone();
        let dependency_handles: Vec<SharedFuture> = dependencies
            .iter()
            .map(|target_name| (self.make_target(target_name, depth + 1)))
            .collect();
        let makefile_path_owned = self.makefile_path.to_owned();
        let target_name_owned = target_name.clone();
        let multi_progress_owned = self.multi_progress.clone();

        let progress_bar = ProgressBar::new(2);
        let progress_bar = multi_progress_owned.insert_from_back(0, progress_bar);
        let join_handle = task::spawn(async move {
            let progress_bar = progress_bar.with_finish(ProgressFinish::AndLeave);
            let indentation = match depth {
                0 => "".to_owned(),
                depth => format!("{}{} ", " ".repeat(depth - 1), "↱"),
            };
            progress_bar.set_prefix(format!("{}{}", indentation, target_name_owned));
            progress_bar.set_message("Running…");
            progress_bar.set_position(0);
            progress_bar.set_style(
                ProgressStyle::with_template("⏳|   ⋯ | {prefix:20}")
                    .expect("Could not construct progress bar."),
            );

            join_all(dependency_handles).await;
            progress_bar.set_position(1);
            progress_bar.set_style(
                ProgressStyle::with_template("{spinner} | {elapsed:>03} | {prefix:20}")
                    .expect("Could not construct progress bar."),
            );
            progress_bar.enable_steady_tick(Duration::from_millis(16));

            make_individual_dependency(dependencies, &makefile_path_owned, &target_name_owned);
            progress_bar.set_position(2);
            progress_bar.set_style(
                ProgressStyle::with_template("✅| {elapsed:>03} | {prefix:20}")
                    .expect("Could not construct progress bar."),
            );
        });
        let join_handle = join_handle.shared();
        self.futures
            .insert(target_name.clone(), join_handle.clone());
        join_handle
    }
}

fn make_individual_dependency(
    dependencies: Vec<TargetName>,
    makefile_path: &Path,
    target_name: &TargetName,
) {
    let makefile_path_str = &makefile_path.to_string_lossy();
    let mut args = vec!["-f", makefile_path_str, &target_name.0];

    for dependency in &dependencies {
        args.push("-o");
        args.push(&dependency.0);
    }

    // println!("[{}] Starting…", target_name);
    let _ = Command::new("make")
        .args(args)
        .output()
        .expect("failed to execute process");
    // println!("[{}] Finished.", target_name);
    // dbg!(output);
}
