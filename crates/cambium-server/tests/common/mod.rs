use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};

use cambium_backend_enroute::EnrouteBackend;

pub struct Server {
    child: Child,
    _directory: tempfile::TempDir,
    pub git: String,
    pub backend: EnrouteBackend,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    pub async fn start() -> Self {
        let binary =
            std::env::var("ENROUTE_BIN").expect("ENROUTE_BIN must name the patched binary");
        let directory = tempfile::tempdir().unwrap();
        let sockets: Vec<_> = (0..3)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let ports: Vec<_> = sockets
            .iter()
            .map(|socket| socket.local_addr().unwrap().port())
            .collect();
        let api = format!("http://127.0.0.1:{}", ports[0]);
        let git = format!("http://127.0.0.1:{}/", ports[1]);
        let config = directory.path().join("enroute.toml");
        std::fs::write(&config, format!(
            "[bucket]\nuri = 'file://{root}/objects'\n[node]\ndata_dir = '{root}/node'\ninternal_listen = '127.0.0.1:{internal}'\ncpu_threads = 2\n[listen]\napi = '127.0.0.1:{api}'\ngit = '127.0.0.1:{git}'\n[git]\nallow_private_remotes = true\n",
            root = directory.path().display(), internal = ports[2], api = ports[0], git = ports[1],
        )).unwrap();
        drop(sockets);
        let mut child = Command::new(binary)
            .args(["--config", &format!("file://{}", config.display()), "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if let Ok(backend) = EnrouteBackend::connect(&api, &git, None).await {
                return Self {
                    child,
                    _directory: directory,
                    git,
                    backend,
                };
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "Enroute exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("Enroute did not become ready");
    }
}
