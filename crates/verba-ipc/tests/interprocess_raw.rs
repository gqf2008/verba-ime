//! 原始 interprocess local_socket 回显测试（隔离 verba-ipc 封装）。

use std::io::{Read, Write};
use std::sync::mpsc;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ConnectOptions, GenericNamespaced, ListenerOptions};
use interprocess::ConnectWaitMode;

#[test]
fn raw_sync_echo() {
    let name = format!("verba-raw-{}", std::process::id());
    let server_name = name.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let n = server_name.to_ns_name::<GenericNamespaced>().unwrap();
        let listener = ListenerOptions::new().name(n).create_sync().unwrap();
        ready_tx.send(()).unwrap();
        let mut conn = listener.accept().unwrap();
        let mut buf = [0u8; 4];
        conn.read_exact(&mut buf).unwrap();
        conn.write_all(&buf).unwrap();
    });
    ready_rx.recv().unwrap();
    let n = name.to_ns_name::<GenericNamespaced>().unwrap();
    let mut client = ConnectOptions::new()
        .name(n)
        .wait_mode(ConnectWaitMode::Unbounded)
        .connect_sync()
        .unwrap();
    client.write_all(b"ping").unwrap();
    let mut buf = [0u8; 4];
    client.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ping");
    server.join().unwrap();
}
