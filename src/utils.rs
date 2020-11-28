use failure::Error;
use futures::future::Future;
use rand::distributions::Alphanumeric;
use rand::Rng;
use retry::delay::{jitter, Exponential};

use std::fs;
use std::path::Path;
use std::time::Duration;

// returns a Vec of 5 durations with a random jitter
fn random_durations() -> Vec<Duration> {
    Exponential::from_millis(2)
        .map(jitter)
        .map(|x| x * 100)
        .take(5)
        .collect()
}

// takes a Fn closure that returns a Result<T, Error>
// calls the closure asynchronously until it either returns Ok or fails enough times
pub async fn exponential_retry_async<C, F, T>(closure: C) -> Result<T, Error>
where
    C: Fn() -> F,
    F: Future<Output = Result<T, Error>>,
{
    let mut err = None;
    for duration in random_durations() {
        tokio::time::delay_for(duration).await;
        match closure().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                err = Some(e);
            }
        }
    }

    Err(err.unwrap())
}

// generates a random alphanumeric string (to be used as a filename)
pub fn random_string(size: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(size)
        .collect::<String>()
}

// check if a file exists in the filesystem
pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

// reads a file from the filesystem
pub fn read_file(filename: &str) -> Vec<u8> {
    fs::read(&filename).unwrap()
}

// deletes a file from the filesystem
pub fn delete_file(path: &str) {
    if file_exists(&path) {
        fs::remove_file(&path).expect("Couldn't remove file");
    }
}
