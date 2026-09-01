//! Getting the model pack, so that using CodeGloss does not start with Python.
//!
//! The weights are CC-BY-SA-4.0 and this repository is MIT, so they are not
//! here and never will be (AGENTS.md). They live in their own repository, as
//! release assets, and this module fetches them into the same cache directory
//! the glosses go to.
//!
//! IMPORTANT: the download is never on the path of a request, and never on the
//! path of `initialize` either. 120 MB over a network is minutes, and a
//! language server that does not answer `initialize` looks broken rather than
//! busy. It is a subcommand - `codegloss-lsp --fetch-model` - that a person or
//! an installer runs once; the server itself only ever looks for a pack that
//! is already there.
//!
//! What is trusted, and what is checked:
//!
//! - `manifest.json` is fetched first and trusted because of where it came
//!   from: HTTPS, from a release named by [`DEFAULT_BASE_URL`] compiled into
//!   this binary. Nothing else about it is verified.
//! - **Every other file is checked against it** - length and SHA-256 - by
//!   [`Manifest::verify`]. That is what catches the failure that actually
//!   happens: a download that stopped early, or a pack a killed process left
//!   half-written. Bad weights do not produce an error, they produce fluent
//!   nonsense, so they have to be caught before they are used.
//! - The pack's `model_version` must be the one this build expects
//!   ([`EXPECTED_MODEL_VERSION`]), because it goes into every cache key.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codegloss_translator::{MANIFEST_FILE, Manifest};

/// Where packs are published.
///
/// A repository of its own, so that this MIT repository holds no CC-BY-SA
/// asset at all - not in the tree, not as a release asset.
pub const DEFAULT_BASE_URL: &str =
    "https://github.com/shutx-net/codegloss-models/releases/download/fugumt-en-ja-1";

/// The pack this build is built against.
///
/// It is not a preference: `model_version` is part of every cache key, and a
/// pack that says something else would answer with glosses this build cannot
/// look up. Changing the published pack is therefore a change here too.
pub const EXPECTED_MODEL_VERSION: &str = "fugumt-en-ja-8b2d3d3b7da2";

/// Flag that runs the download instead of the server.
pub const FETCH_FLAG: &str = "--fetch-model";
/// Overrides [`DEFAULT_BASE_URL`], for testing against a local server and for
/// a mirror.
pub const BASE_URL_VARIABLE: &str = "CODEGLOSS_MODEL_URL";

/// Where to fetch from: [`BASE_URL_VARIABLE`] when it is set, and
/// [`DEFAULT_BASE_URL`] otherwise.
///
/// Read here rather than inside [`fetch`] so that `fetch` takes what it needs
/// as an argument. A test that had to set an environment variable to point the
/// download somewhere would be setting it for every other test in the process,
/// and `std::env::set_var` is unsafe in this edition, which this crate forbids.
pub fn base_url() -> String {
    std::env::var(BASE_URL_VARIABLE)
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned())
}

/// How long a single file transfer may stall before it is given up on.
///
/// A whole-transfer deadline would have to be generous enough for 120 MB on a
/// slow line, which is too generous to catch anything. A stalled read is the
/// failure worth noticing.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Where a downloaded pack lives, under the cache root.
///
/// Named after the version so that two of them can sit side by side: a build
/// that expects one must not find the other and load it.
pub fn directory(cache: &Path) -> PathBuf {
    cache.join("model-packs").join(EXPECTED_MODEL_VERSION)
}

/// The pack in `cache`, if it is there and complete.
///
/// A pack that fails its manifest is not a pack: it is reported as missing, so
/// that the caller falls back to English rather than translating with weights
/// that may be half a file.
pub fn installed(cache: &Path) -> Option<PathBuf> {
    let directory = directory(cache);
    let manifest = Manifest::read_from(&directory).ok()?;
    match manifest.verify(&directory) {
        Ok(()) => Some(directory),
        Err(error) => {
            tracing::warn!(
                pack = %directory.display(),
                "the downloaded model pack did not match its manifest, ignoring it: {error:#}"
            );
            None
        }
    }
}

/// Downloads the pack into `cache`, replacing whatever was there.
///
/// Files land in a directory of their own and are moved into place only once
/// every one of them has been checked, so an interrupted download leaves the
/// previous pack alone rather than half of a new one.
pub fn fetch(cache: &Path, base: &str) -> anyhow::Result<PathBuf> {
    let base = base.trim_end_matches('/');

    let manifest_text = get(&format!("{base}/{MANIFEST_FILE}"))?;
    let manifest = Manifest::parse(&manifest_text)?;
    if manifest.model_version != EXPECTED_MODEL_VERSION {
        anyhow::bail!(
            "{base} publishes {}, and this build is built against {EXPECTED_MODEL_VERSION}",
            manifest.model_version
        );
    }

    let final_directory = directory(cache);
    let staging = final_directory.with_extension("partial");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    tracing::info!(
        model = %manifest.model_id,
        license = %manifest.license,
        files = manifest.files.len(),
        "downloading the model pack"
    );
    fs::write(staging.join(MANIFEST_FILE), &manifest_text)?;
    for name in manifest.files.keys() {
        if name == MANIFEST_FILE {
            continue;
        }
        tracing::info!(file = %name, "downloading");
        download(&format!("{base}/{name}"), &staging.join(name))?;
    }

    manifest.verify(&staging)?;

    let _ = fs::remove_dir_all(&final_directory);
    if let Some(parent) = final_directory.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&staging, &final_directory)?;

    // The licence travels with the weights, and a person who ran a one-line
    // command deserves to be told what they now have.
    tracing::info!(
        pack = %final_directory.display(),
        "the model pack is installed: {} ({}). {}",
        manifest.model_id,
        manifest.license,
        manifest.attribution
    );
    Ok(final_directory)
}

fn get(url: &str) -> anyhow::Result<String> {
    Ok(agent().get(url).call()?.body_mut().read_to_string()?)
}

fn download(url: &str, into: &Path) -> anyhow::Result<()> {
    let mut response = agent().get(url).call()?;
    let mut file = fs::File::create(into)?;
    io::copy(&mut response.body_mut().as_reader(), &mut file)?;
    Ok(())
}

fn agent() -> ureq::Agent {
    // The operating system's certificate store, not the set compiled into the
    // binary. A machine behind a TLS-inspecting proxy - a corporate network,
    // and the environment this was developed in - presents a certificate whose
    // issuer is in the system store and in no compiled-in set, and the download
    // fails with `UnknownIssuer`. Trusting what the machine already trusts is
    // what makes the download work where everything else on that machine does.
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();

    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_recv_body(Some(READ_TIMEOUT))
        .user_agent(concat!("codegloss-lsp/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::*;

    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};

        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// A pack of two tiny files, with a manifest that describes them.
    fn pack(version: &str) -> Vec<(String, Vec<u8>)> {
        let notice = b"NOTICE".to_vec();
        let weights = b"not really 120 MB".to_vec();
        let manifest = format!(
            r#"{{"model_id":"staka/fugumt-en-ja","model_version":"{version}",
                "license":"CC-BY-SA-4.0","attribution":"test","files":{{
                "NOTICE":{{"sha256":"{}","bytes":{}}},
                "pytorch_model.bin":{{"sha256":"{}","bytes":{}}}}}}}"#,
            sha256(&notice),
            notice.len(),
            sha256(&weights),
            weights.len()
        );
        vec![
            (MANIFEST_FILE.to_owned(), manifest.into_bytes()),
            ("NOTICE".to_owned(), notice),
            ("pytorch_model.bin".to_owned(), weights),
        ]
    }

    /// Serves `files` at `http://127.0.0.1:<port>/<name>` until dropped.
    ///
    /// A handful of lines of HTTP rather than a dependency: the download only
    /// needs `GET`, and a test server that speaks exactly what is under test
    /// cannot drift from it.
    fn serve(files: Vec<(String, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                answer(&mut stream, &files);
            }
        });
        base
    }

    fn answer(stream: &mut TcpStream, files: &[(String, Vec<u8>)]) {
        let mut request = String::new();
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        if reader.read_line(&mut request).is_err() {
            return;
        }
        // Drain the headers; the body is never anything but empty.
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
            line.clear();
        }

        let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
        let name = path.trim_start_matches('/');
        match files.iter().find(|(candidate, _)| candidate == name) {
            Some((_, body)) => {
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
            None => {
                let _ = write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        }
        let _ = stream.flush();
    }

    fn cache(test: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("codegloss-fetch-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_published_pack_is_downloaded_and_verified() {
        let base = serve(pack(EXPECTED_MODEL_VERSION));
        let cache = cache("ok");
        let pack = fetch(&cache, &base).expect("the pack downloads");
        assert_eq!(pack, directory(&cache));
        assert!(pack.join("pytorch_model.bin").is_file());
        assert_eq!(super::installed(&cache).as_deref(), Some(pack.as_path()));
        let _ = fs::remove_dir_all(&cache);
    }

    /// A pack of the wrong version would answer with glosses this build cannot
    /// look up, because `model_version` is part of every cache key.
    #[test]
    fn a_pack_of_another_version_is_refused() {
        let base = serve(pack("fugumt-en-ja-something-else"));
        let cache = cache("version");
        let error = fetch(&cache, &base).expect_err("the version does not match");

        assert!(
            format!("{error}").contains(EXPECTED_MODEL_VERSION),
            "{error}"
        );
        assert!(
            !directory(&cache).exists(),
            "nothing should have been installed"
        );
        let _ = fs::remove_dir_all(&cache);
    }

    /// The failure that actually happens: bytes that stopped early. Nothing
    /// may be installed, because half a weight file translates to nonsense
    /// rather than to an error.
    #[test]
    fn a_truncated_download_installs_nothing() {
        let mut files = pack(EXPECTED_MODEL_VERSION);
        files
            .iter_mut()
            .find(|(name, _)| name == "pytorch_model.bin")
            .expect("the weights are in the pack")
            .1
            .truncate(3);
        let base = serve(files);
        let cache = cache("truncated");

        let error = fetch(&cache, &base).expect_err("the digest does not match");
        assert!(format!("{error}").contains("bytes"), "{error}");
        assert!(
            !directory(&cache).exists(),
            "nothing should have been installed"
        );
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn nothing_downloaded_is_nothing_installed() {
        assert_eq!(super::installed(&cache("empty")), None);
    }

    #[test]
    fn the_base_url_defaults_to_where_packs_are_published() {
        assert_eq!(base_url(), DEFAULT_BASE_URL);
    }
}
