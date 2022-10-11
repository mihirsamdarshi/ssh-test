# Rust SSH Testing

## Description

Demonstrates code for creating a local port forward in Rust, replicating the command line `ssh -L` feature

### Quickstart

#### SSH Tunnel
Requires [Rust](https://rustup.rs)

Set the `const`s at the top of the `main.rs` file to your own values, then run the app. 
I have set a timeout for the app to exit after 0 minutes but that could be easily changed
```bash
$ cd async-ssh2-lite
$ cargo run
```

#### Server
Requires [NodeJS](https://nodejs.dev) and `yarn` (`npm install -g yarn`)

Install dependencies and run web app
```bash
$ cd web
$ yarn install
$ yarn run start
```

#### Server
Preferably run this part on a remote host. Requires Python3.

Install a venv, install required packages, activate the venv, and run the server 
```bash
$ cd server
$ python3 -m venv venv
$ source venv/bin/activate
$ python3 app.py
```

