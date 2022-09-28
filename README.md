# ssh-test
Testing out creating local port forwarding using Russh

```md
ssh-test 0.1.0
Simple program to forward a local port to a remote port on a remote host

USAGE:
    ssh-test --user <USER> --ip <IP> --remote-port <REMOTE_PORT> --local-port <LOCAL_PORT>

OPTIONS:
    -h, --help                         Print help information
    -i, --ip <IP>                      The IPV4 address of the remote host (e.g. 80.69.420.85)
    -l, --local-port <LOCAL_PORT>      The local port to listen on (e.g 9876)
    -r, --remote-port <REMOTE_PORT>    The port on the remote host to connect to (e.g. 8000)
    -u, --user <USER>                  The username to connect as on the remote host (e.g. root)
    -V, --version                      Print version information
```

To use, clone and enter this repo, then run `cargo run -- --user <USER> --ip <IP> --remote-port <REMOTE_PORT> --local-port <LOCAL_PORT>`

