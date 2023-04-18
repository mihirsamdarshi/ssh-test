# Local Port Forwarding Experiments

This repository contains experiments that I have tried while trying to get port forwarding working with Rust.

Specifically this repository contains experiments with two crates thus far:

- [russh](https://github.com/warp-tech/russh) - An (updated) fork of Thrussh, which is a pure-Rust implementation of the
  SSH protocol.
- [ssh2-rs](https://github.com/alexcrichton/ssh2-rs) - A Rust wrapper around libssh2.
- [async-ssh2-lite](https://github.com/bk-rs/ssh-rs) - An async wrapper around ssh2-rs, which is a Rust wrapper around
  libssh2.

In addition, in order to test the capabilities of both these libraries, this repository contains a demo web application
that loads some data and sends it to the frontend. That code is located in the webapp directory.

I find that running this is easy, however, for just a frontend, I have also created a Docker image that runs the Gatsby
Gitbook starter, hosted on Docker Hub
at [mihirstanford/gatsby-0gitbook-starter](https://hub.docker.com/r/mihirstanford/gatsby-gitbook-starter).

### NOTE:

None of the binaries work as expected, and struggle to handle a full remote port forwarding session. I have not yet
debugged the issues in either library, and am working on fixing/upstreaming the fixes that I make to whichever library I
get working. Therefore, this repository is a work in progress, and I will be updating it as I work further.

## Example

To run a demo web application on your local computer and connect to it via SSH, ensure that you have Docker and are
running an SSH server on your local machine. Add your own SSH public key to `~/.ssh/authorized_keys`. Then, run the
following commands:

```bash
docker run -d -p 8080:8080 --rm mihirstanford/gatsby-gitbook-starter
```

Then, you may navigate into either the `async-ssh2-lite` or `russh` directories and run the following:

```bash
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

First, go to Settings > General > Sharing and enable Remote Login. You may need to restart your computer.

To create an SSH key and add it to your `authorized_keys` file, run the following commands:

```bash
# if you don't have an SSH key, create one
ssh-keygen -t ed25519 -C "<your email here>"

# add your SSH key to your authorized_keys file
touch ~/.ssh/authorized_keys
cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys
```
