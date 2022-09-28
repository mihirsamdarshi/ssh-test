# ssh-test

Testing out creating local port forwarding using Russh.

**This is a work in progress, and does not currently work to forward many concurrent TCP streams (like loading a
webpage)**

```md
ssh-test 0.1.0
Simple program to forward a local port to a remote port on a remote host

USAGE:
ssh-test --user <USER> --ip <IP> --remote-port <REMOTE_PORT> --local-port <LOCAL_PORT>

OPTIONS:
-h, --help Print help information
-i, --ip <IP>                      The IPV4 address of the remote host (e.g. 80.69.420.85)
-l, --local-port <LOCAL_PORT>      The local port to listen on (e.g 9876)
-r, --remote-port <REMOTE_PORT>    The port on the remote host to connect to (e.g. 8000)
-u, --user <USER>                  The username to connect as on the remote host (e.g. root)
-V, --version Print version information
```

To use, clone and enter this repo, then
run `cargo run -- --user <USER> --ip <IP> --remote-port <REMOTE_PORT> --local-port <LOCAL_PORT>`

## Example

To run a demo web application on your local computer and connect to it via SSH, ensure that you have Docker and are
running an SSH server on your local machine. Add your own SSH public key to `~/.ssh/authorized_keys`. Then, run the
following commands:

```bash
docker run docker run -d -p 8080:8080 --rm mihirstanford/gatsby-gitbook-starter
cargo run -- --user <USER> --ip 127.0.0.1 --remote-port 8080 --local-port 42069
```

Then, in your browser, navigate to `localhost:42069` and you should see the demo web application fail to load locally

Verify that OpenSSH works by running the following command:

```bash
ssh -L 42070:localhost:8080 <USER>@127.0.0.1
```

Then, in your browser, navigate to `localhost:42070` and you should see the demo web application successfully load
locally

### To Enable SSH Login (On Mac)

First, go to System Preferences > Sharing and enable Remote Login. You may need to restart your computer.

To create an SSH key and add it to your `authorized_keys` file, run the following commands:

```bash
# if you don't have an SSH key, create one
ssh-keygen -t ed25519 -C "<your email here>"

# add your SSH key to your authorized_keys file
touch ~/.ssh/authorized_keys
cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys
```