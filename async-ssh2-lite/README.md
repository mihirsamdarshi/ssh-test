# async-ssh2-lite Port Forwarding

## Description

Demonstrates code for creating a local port forward in Rust using the `async-ssh2-lite` library, replicating the command
line `ssh -L` feature

### Quickstart

#### SSH Tunnel

```bash
$ cd async-ssh2-lite
$ cargo run -- --user <USER> --ip 127.0.0.1 --remote-port 8080 --local-port 42069
```

