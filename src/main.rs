#![recursion_limit = "1024"]
#[macro_use]
extern crate error_chain;
extern crate itertools;
extern crate rusoto_core;
extern crate rusoto_s3;

use rusoto_core::Region;
use rusoto_s3::S3Client;

use itertools::Itertools;
use tempfile::Builder;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::io::Write;

pub mod errors {
    error_chain! {}
}
use errors::*;
mod git;
mod s3;  // 移除了 gpg 模块

quick_main!(run);

struct Settings {
    remote_alias: String,
    root: s3::Key,
}

fn run() -> Result<()> {
    let region = Region::Custom {
        name: String::from("us-east-1"),
        endpoint: env::var("S3_ENDPOINT").unwrap_or("http://localhost:5246".to_string()),
    };

    let s3 = S3Client::new(region);

    let mut args = env::args();
    args.next();
    let alias = args.next().chain_err(|| "must provide alias")?;
    let url = args.next().chain_err(|| "must provide url")?;

    let (bucket, path) = {
        if !url.starts_with("s3://") {
            bail!("remote url does not start with s3://. expected a url in the format s3://bucket/prefix")
        }
        let url = &url[5..];
        let slash = match url.find('/') {
            Some(idx) => idx,
            None => {
                bail!("remote url does not appear to have a prefix. expected a url in the format s3://bucket/prefix");
            }
        };
        let bucket = url.get(..slash).unwrap();
        let end = if url.ends_with('/') {
            url.len() - 1
        } else {
            url.len()
        };
        let path = url.get((slash + 1)..end).unwrap();
        (bucket, path)
    };

    let git_dir = PathBuf::from(env::var("GIT_DIR").chain_err(|| "GIT_DIR not set")?);
    let cur_dir = env::current_dir().chain_err(|| "could not get pwd")?;
    let work_dir = cur_dir.join(&git_dir).join("remote-s3").join(&alias);

    fs::create_dir_all(&work_dir)
        .chain_err(|| format!("could not create work dir: {:?}", work_dir))?;

    let settings = Settings {
        remote_alias: alias,
        root: s3::Key {
            bucket: bucket.to_string(),
            key: path.to_string(),
        },
    };

    cmd_loop(&s3, &settings)
}

#[derive(Debug)]
struct GitRef {
    name: String,
    sha: String,
}

impl GitRef {
    fn bundle_path(&self, root: String) -> String {
        let mut s = String::new();
        s.push_str(&format!("{}/{}/{}.bundle", &root, &self.name, &self.sha));
        s
    }
}

#[derive(Debug)]
struct RemoteRef {
    object: s3::Key,
    updated: String,
    reference: GitRef,
}

#[derive(Debug)]
struct RemoteRefs {
    by_update_time: Vec<RemoteRef>,
}

impl RemoteRefs {
    fn latest_ref(&self) -> &RemoteRef {
        self.by_update_time.get(0).unwrap()
    }
}

fn fetch_from_s3(s3: &S3Client, settings: &Settings, r: &GitRef) -> Result<()> {
    let tmp_dir = Builder::new()
        .prefix("s3_fetch")
        .tempdir()
        .chain_err(|| "mktemp dir failed")?;
    let bundle_file = tmp_dir.path().join("bundle");

    let path = r.bundle_path(settings.root.key.to_owned());
    let o = s3::Key {
        bucket: settings.root.bucket.to_owned(),
        key: path,
    };
    
    // 直接下载 bundle 文件，不进行解密
    s3::get(s3, &o, &bundle_file)?;

    git::bundle_unbundle(&bundle_file, &r.name)?;

    Ok(())
}

fn push_to_s3(s3: &S3Client, settings: &Settings, r: &GitRef) -> Result<()> {
    let tmp_dir = Builder::new()
        .prefix("s3_push")
        .tempdir()
        .chain_err(|| "mktemp dir failed")?;
    let bundle_file = tmp_dir.path().join("bundle");

    git::bundle_create(&bundle_file, &r.name)?;

    let path = r.bundle_path(settings.root.key.to_owned());
    let o = s3::Key {
        bucket: settings.root.bucket.to_owned(),
        key: path,
    };
    // 直接上传 bundle 文件，不进行加密
    s3::put(s3, &bundle_file, &o)?;

    Ok(())
}

fn cmd_fetch(s3: &S3Client, settings: &Settings, sha: &str, name: &str) -> Result<()> {
    if name == "HEAD" {
        // Ignore head, as it's guaranteed to point to a ref we already downloaded
        return Ok(());
    }
    let git_ref = GitRef {
        name: name.to_string(),
        sha: sha.to_string(),
    };
    fetch_from_s3(s3, settings, &git_ref)?;
    let _ = writeln!(io::stdout());
    Ok(())
}

fn cmd_push(s3: &S3Client, settings: &Settings, push_ref: &str) -> Result<()> {
    let force = push_ref.starts_with('+');
    let mut split = push_ref.split(':');

    let src_ref = split.next().ok_or("Invalid push reference format")?;
    let src_ref = if force { &src_ref[1..] } else { src_ref };
    let dst_ref = split.next().ok_or("Invalid push reference format")?;

    if src_ref != dst_ref {
        println!("error src_ref != dst_ref");
        println!();
        return Ok(());
    }

    match list_remote_refs(s3, settings) {
        Ok(all_remote_refs) => {
            let remote_refs = all_remote_refs.get(src_ref);
            let prev_ref = remote_refs.map(|rs| rs.latest_ref());
            let local_sha = git::rev_parse(src_ref)?;
            let local_ref = GitRef {
                name: src_ref.to_string(),
                sha: local_sha,
            };

            let can_push = force || match prev_ref {
                Some(prev_ref) => {
                    if !git::is_ancestor(&local_ref.sha, &prev_ref.reference.sha)? {
                        println!("error {} remote changed: force push to add new ref, the old ref will be kept until its merged)", dst_ref);
                        println!();
                        false
                    } else {
                        true
                    }
                }
                None => true,
            };

            if can_push {
                if let Err(e) = push_to_s3(s3, settings, &local_ref) {
                    println!("error failed to push: {}", e);
                    println!();
                    return Ok(());
                }

                // Delete any ref that is an ancestor of the one we pushed
                if let Some(remote_refs) = remote_refs {
                    for r in remote_refs.by_update_time.iter() {
                        if git::is_ancestor(&local_ref.sha, &r.reference.sha)? {
                            if let Err(e) = s3::del(s3, &r.object) {
                                println!("warning failed to delete old ref: {}", e);
                            }
                        }
                    }
                }
                let _ = writeln!(io::stdout(), "ok {}", dst_ref);
            }
        }
        Err(e) => {
            println!("error failed to list remote refs: {}", e);
        }
    }
    writeln!(io::stdout());
    Ok(())
}

// Implement protocol defined here:
// https://github.com/git/git/blob/master/Documentation/gitremote-helpers.txt
 fn cmd_loop(s3: &S3Client, settings: &Settings) -> Result<()> {
    loop {
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() || input.is_empty() {
            return Ok(());
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        let mut iter = input.split_ascii_whitespace();
        let cmd = iter.next();
        let arg1 = iter.next();
        let arg2 = iter.next();
        
        let result = match (cmd, arg1, arg2) {
            (Some("push"), Some(ref_arg), None) => cmd_push(s3, settings, ref_arg),
            (Some("fetch"), Some(sha), Some(name)) => cmd_fetch(s3, settings, sha, name),
            (Some("capabilities"), None, None) => cmd_capabilities(),
            (Some("list"), None, None) => cmd_list(s3, settings),
            (Some("list"), Some("for-push"), None) => cmd_list(s3, settings),
            (None, None, None) => return Ok(()),
            _ => {
                println!("error unknown command");
                println!();
                Ok(())
            }
        };

        if let Err(e) = result {
            println!("error {}", e);
            println!();
        }
    }
}

fn cmd_unknown() -> Result<()> {
    println!("unknown command");
    println!();
    Ok(())
}

fn list_remote_refs(s3: &S3Client, settings: &Settings) -> Result<HashMap<String, RemoteRefs>> {
    let result = s3::list(s3, &settings.root)?;
    let objects = match result.contents {
        Some(l) => l,
        None => vec![],
    };
    let map: HashMap<String, Vec<RemoteRef>> = objects
        .into_iter()
        .flat_map(|o| {
            o.key.to_owned().map(|k| {
                let last_slash = k.rfind('/').unwrap();
                let last_dot = k.rfind('.').unwrap();
                let name = k
                    .get((settings.root.key.len() + 1)..last_slash)
                    .unwrap()
                    .to_string();
                let sha = k.get((last_slash + 1)..last_dot).unwrap().to_string();
                (
                    name.to_owned(),
                    RemoteRef {
                        object: s3::Key {
                            bucket: settings.root.bucket.to_owned(),
                            key: k,
                        },
                        updated: o.last_modified.unwrap(),
                        reference: GitRef { name, sha },
                    },
                )
            })
        })
        .into_group_map();
    let refs: HashMap<String, RemoteRefs> = map
        .into_iter()
        .map(|(name, refs)| (name, sorted_remote_refs(refs)))
        .collect();
    Ok(refs)
}

fn sorted_remote_refs(refs: Vec<RemoteRef>) -> RemoteRefs {
    RemoteRefs {
        by_update_time: refs
            .into_iter()
            .sorted_by_key(|i| i.updated.to_owned())
            .rev()
            .collect(),
    }
}

fn cmd_list(s3: &S3Client, settings: &Settings) -> Result<()> {
    let refs = list_remote_refs(s3, settings)?;
    if !refs.is_empty() {
        for (_name, refs) in refs.iter() {
            let mut iter = refs.by_update_time.iter();
            let latest = iter.next().unwrap();
            writeln!(io::stdout(), "{} {}", latest.reference.sha, latest.reference.name);

            for stale_ref in iter {
                let short_sha = stale_ref.reference.sha.get(0..7).unwrap();
                writeln!(io::stdout(),
                    "{} {}__{}",
                    stale_ref.reference.sha, stale_ref.reference.name, short_sha
                );
            }
        }
        // Advertise the HEAD as being the latest master ref
        // this is needed, as git clone checks outs the HEAD
        if refs.contains_key("refs/heads/master") {
            writeln!(io::stdout(), "@refs/heads/master HEAD");
        }
    }
    writeln!(io::stdout());
    Ok(())
}

fn cmd_capabilities() -> Result<()> {
    println!("*push");
    println!("*fetch");
    println!();
    Ok(())
}
