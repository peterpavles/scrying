/*
 *   This file is part of NCC Group Scamper https://github.com/nccgroup/scamper
 *   Copyright 2020 David Young <david(dot)young(at)nccgroup(dot)com>
 *   Released as open source by NCC Group Plc - https://www.nccgroup.com
 *
 *   Scamper is free software: you can redistribute it and/or modify
 *   it under the terms of the GNU General Public License as published by
 *   the Free Software Foundation, either version 3 of the License, or
 *   (at your option) any later version.
 *
 *   Scamper is distributed in the hope that it will be useful,
 *   but WITHOUT ANY WARRANTY; without even the implied warranty of
 *   MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *   GNU General Public License for more details.
 *
 *   You should have received a copy of the GNU General Public License
 *   along with Scamper.  If not, see <https://www.gnu.org/licenses/>.
*/

use error::Error;
use std::fs::create_dir_all;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
//use argparse::Mode;
#[allow(unused)]
use log::{debug, error, info, trace, warn};
use parsing::{generate_target_lists, InputLists};
use simplelog::{
    CombinedLogger, Config, LevelFilter, SharedLogger, TermLogger,
    TerminalMode, WriteLogger,
};
use std::fs::File;
use std::sync::Arc;

#[cfg(feature = "headlesschrome")]
use headless_chrome::Browser;

#[cfg(feature = "wkhtmltoimage")]
use wkhtmltopdf::ImageApplication;
mod argparse;
mod error;
mod parsing;
mod rdp;
mod util;
mod web;

pub enum ThreadStatus {
    Complete,
}

fn main() {
    println!("Starting NCC Group Scamper...");
    let opts = argparse::parse();

    // Configure logging
    let mut log_dests: Vec<Box<dyn SharedLogger>> = Vec::new();

    if let Some(log_file) = &opts.log_file {
        // Enable logging to a file at INFO level by default
        // Increasing global log verbosity increases log file verbosity
        // accordingly. Combinations such as --silent -vv make sense
        // when using a log file as the file will get TRACE messages
        // while the terminal only gets WARN and higher.
        let level_filter = match opts.verbose {
            0 => LevelFilter::Info,
            1 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        };
        log_dests.push(WriteLogger::new(
            level_filter,
            Config::default(),
            File::create(log_file).unwrap(),
        ));
    }

    let level_filter = if !opts.silent {
        match opts.verbose {
            0 => LevelFilter::Info,
            1 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    } else {
        LevelFilter::Warn
    };

    log_dests.push(TermLogger::new(
        level_filter,
        Config::default(),
        TerminalMode::Mixed,
    ));

    CombinedLogger::init(log_dests).unwrap();

    debug!("Got opts:\n{:?}", opts);

    // Load in the target lists, parsed from arguments, files, and nmap
    let targets = Arc::new(generate_target_lists(&opts));
    println!("target list: {:?}", targets);

    // Create output directories if they do not exist
    let rdp_output_dir = Path::new("./output/rdp");
    let web_output_dir = Path::new("./output/web");
    if !rdp_output_dir.is_dir() {
        create_dir_all(rdp_output_dir).unwrap_or_else(|_| {
            panic!("Error creating directory {}", rdp_output_dir.display())
        });
    }
    if !web_output_dir.is_dir() {
        create_dir_all(web_output_dir).unwrap_or_else(|_| {
            panic!("Error creating directory {}", web_output_dir.display())
        });
    }

    // Spawn tokio workers to iterate over the targets
    //let rdp_output_dir_arc = Arc::new(rdp_output_dir);
    let targets_clone = targets.clone();
    let rdp_handle = thread::spawn(move || {
        debug!("Starting RDP worker threads");
        rdp_worker(targets_clone, rdp_output_dir)
    });
    // clone here will be more useful when there are more target types
    let targets_clone = targets; //.clone();
    let web_handle = thread::spawn(move || {
        debug!("Starting Web worker threads");
        web_worker(targets_clone, &web_output_dir).unwrap()
    });

    // wait for the workers to complete
    rdp_handle.join().unwrap().unwrap();
    web_handle.join().unwrap();
}

fn rdp_worker(
    targets: Arc<InputLists>,
    output_dir: &'static Path,
) -> Result<(), ()> {
    use mpsc::{Receiver, Sender};
    let max_workers: usize = 3;
    let mut num_workers: usize = 0;
    let mut targets_iter = targets.rdp_targets.iter();
    let mut workers: Vec<_> = Vec::new();
    let (thread_status_tx, thread_status_rx): (
        Sender<ThreadStatus>,
        Receiver<ThreadStatus>,
    ) = mpsc::channel();
    loop {
        // check for status messages
        // Turn off clippy's single_match warning here because match
        // matches the intuition for how try_recv is processed better
        // than an if let.
        #[allow(clippy::single_match)]
        match thread_status_rx.try_recv() {
            Ok(ThreadStatus::Complete) => {
                info!("Thread complete, yay");
                num_workers -= 1;
            }
            Err(_) => {}
        }
        if num_workers < max_workers {
            if let Some(target) = targets_iter.next() {
                let target = target.clone();
                println!("Adding worker for {:?}", target);
                let tx = thread_status_tx.clone();
                let handle = thread::spawn(move || {
                    rdp::capture(&target, &output_dir, tx)
                });

                workers.push(handle);
                num_workers += 1;
            } else {
                break;
            }
        }
    }
    println!("At the join part");
    for w in workers {
        print!("Joining {:?}", w);
        if w.join().unwrap().is_err() {
            warn!("Thread terminated with error");
        }
    }

    Ok(())
}

fn web_worker(
    targets: Arc<InputLists>,
    output_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Fail if compiled witout the wkhtmltoimage feature
    #[cfg(not(any(
        feature = "wkhtmltoimage",
        feature = "wkhtmltoimage_bin",
        feature = "headlesschrome"
    )))]
    return Err("no");

    // Find the path to the wkhtmltoimage binary
    #[cfg(feature = "wkhtmltoimage_bin")]
    let wkhtmltoimage_path = web::get_wkhtmltoimage_path().unwrap();

    #[cfg(feature = "wkhtmltoimage")]
    let image_app =
        ImageApplication::new().expect("Failed to init image application");

    #[cfg(feature = "headlesschrome")]
    let browser = Browser::default().expect("failed to init chrome");
    let tab = browser.wait_for_initial_tab().expect("Failed to init tab");

    for target in &targets.web_targets {
        #[cfg(feature = "wkhtmltoimage")]
        if let Err(e) = web::capture(target, output_dir, &image_app) {
            match e {
                Error::IoError(e) => {
                    // Should probably abort on an IO error
                    error!("IO error: {}", e);
                    break;
                }
                Error::WkhtmltoimageError(e) => {
                    // non-fatal error, probably just a nonresponsive
                    // server
                    info!("Failed to capture image: {}", e);
                }
            }
        }
        #[cfg(feature = "wkhtmltoimage_bin")]
        web::capture(target, output_dir, &wkhtmltoimage_path).unwrap();

        #[cfg(feature = "headlesschrome")]
        if let Err(e) = web::capture(target, output_dir, &tab) {
            match e {
                Error::IoError(e) => {
                    // Should probably abort on an IO error
                    error!("IO error: {}", e);
                    break;
                }
                Error::ChromeError(e) => {
                    warn!("Failed to capture image: {}", e);
                }
            }
        }
    }
    Ok(())
}
