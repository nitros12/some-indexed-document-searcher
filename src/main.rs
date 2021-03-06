use ctrlc;
use snafu::{ErrorCompat, ResultExt, Snafu};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

mod config;
mod file_collector;
mod indexer;
mod searcher;
mod last_modified_cache;
mod once_every;
mod gui;

#[derive(Debug, Snafu)]
enum SIDSError {
    #[snafu]
    ConfigLoad { source: config::Error },
    #[snafu]
    CollectorError { source: file_collector::Error },
    #[snafu]
    IndexerError { source: indexer::Error },
    #[snafu]
    LastModifiedCacheError { source: last_modified_cache::Error },
}

struct IndexerData {
    file_collector: file_collector::FilesCollectorIteror,
    doc_indexer: indexer::DocIndexer,
    indexed_files: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
}

fn deploy_indexer(mut data: IndexerData) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for file in data.file_collector {
            if let Ok(file) = file {
                data.doc_indexer.add_job(indexer::IndexRequest(file));
                data.indexed_files.fetch_add(1, Ordering::Relaxed);
            }

            if !data.running.load(Ordering::Relaxed) {
                break;
            }
        }

        data.doc_indexer.close();
    })
}

fn deploy_cc_handler() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::Relaxed);
    })
    .expect("Error setting C-c handler");

    running
}

fn main_inner() -> Result<(), SIDSError> {
    let config = config::load_config().context(ConfigLoad)?;

    println!("config: {:#?}", config);

    let modified_cache =
        last_modified_cache::LastModifiedCache::new(&config).context(LastModifiedCacheError)?;

    let mut doc_indexer = indexer::DocIndexer::new(&config).context(IndexerError)?;
    doc_indexer.spawn_workers().context(IndexerError)?;

    let indexer = doc_indexer.indexer().clone();
    let schema = doc_indexer.schema().clone();

    let searcher = searcher::Searcher::new(schema, indexer).unwrap();

    let running = deploy_cc_handler();

    let indexed_files = Arc::new(AtomicUsize::new(modified_cache.len()));

    let indexer_data = IndexerData {
        file_collector: file_collector::collect_files(&config, modified_cache).context(CollectorError)?,
        doc_indexer,
        indexed_files: indexed_files.clone(),
        running: running.clone(),
    };

    let indexer_thread = deploy_indexer(indexer_data);

    gui::spawn(searcher, indexed_files);

    // set running to false when the gui quits
    running.store(false, Ordering::Relaxed);

    let _ = indexer_thread.join();

    Ok(())
}

fn main() {
    if let Err(e) = main_inner() {
        eprintln!("Oops: {}", e);
        if let Some(bt) = ErrorCompat::backtrace(&e) {
            eprintln!("{}", bt);
        }
    }
}
