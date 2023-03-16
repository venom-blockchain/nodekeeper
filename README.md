## stEVER Node Tools &emsp; [![stever-node-tools: rustc 1.65+]][Rust 1.65] [![Workflow badge]][Workflow]

[stever-node-tools: rustc 1.65+]: https://img.shields.io/badge/rustc-1.65+-lightgray.svg
[Rust 1.65]: https://blog.rust-lang.org/2022/11/03/Rust-1.65.0.html
[Workflow badge]: https://img.shields.io/github/actions/workflow/status/broxus/stever-node-tools/master.yml?branch=master
[Workflow]: https://github.com/broxus/stever-node-tools/actions?query=workflow%3Amaster

All-in-one node management tool with stEVER depool support.

## How to install

* Using Debian package
  ```bash
  sudo apt install path/to/stever.deb
  ```

* Using `cargo install`
  ```bash
  # Install deps for this app
  sudo apt install pkg-config libssl-dev

  # Install deps for the node
  sudo apt install libzstd-dev libclang-dev libtcmalloc-minimal4 libprotobuf-dev libgoogle-perftools-dev

  # Install the app
  cargo install --locked --git https://github.com/broxus/stever-node-tools
  ```

  > NOTE: `systemd` configuration is different for cargo installation,
    see **Validation** section for more info.

## How to use

### Validation

For Debian installation:
```bash
# Add current user to the stever group
sudo usermod -a -G stever $USER
# Update groups cache (you can just relogin instead)
newgrp stever

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
# Or update to the latest stable version:
# rustup update stable

# Configure node
stever init

# Start services
sudo systemctl restart ever-validator
sudo systemctl restart ever-validator-manager
```

<details><summary><b>For cargo installation:</b></summary>
<p>

```bash
# Optionally configure root directory:
# export STEVER_ROOT=/var/stever
#
# Or explicitly specify it as a param, e.g.:
# stever --root /var/stever init

# Configure node
stever init

sudo $(which stever) init systemd
```

</p>
</details>
<br>

> NOTE: Make sure you back up your keys after initial configuration!
>
> All keys are stored at `/var/stever/keys/` (or `$HOME/.stever/keys/` by default for the cargo installation).

You can also configure different steps separately:

```bash
# Initialize only node configs
stever init node

# Initialize only contracts
stever init contracts
```

Updating the node:

```bash
stever init node --rebuild
sudo systemctl restart ever-validator
```

### Metrics exporter

```bash
# Metrics exporter as a server
stever exporter --addr 0.0.0.0:10100

# Metrics exporter to the file
stever exporter --file /var/www/node_metrics.txt
```

<details><summary><b>Example metrics</b></summary>
<p>

```
collected_at 1669042606
node_ready 1
node_version_major 0
node_version_minor 51
node_version_patch 1
mc_seqno 155886
mc_time 1669042601
mc_time_diff 5
sc_time_diff 5
in_current_vset{adnl="d5af8f62c027774831aea3fe00d78fc78ed69f233d885382e72f9adefd8c4f05"} 1
in_next_vset 0
```

</p>
</details>

### Seed generator

```bash
# Generate new seed
stever seed generate
#decline weapon swift luggage gorilla odor clown million leaf royal object movie

# Derive keypair from the seed
stever seed generate | stever seed derive
#{
#  "public": "72e8cb80621c41a95da3a004139ceefa39e8709e7a8183ed9ad601ce9a13714d",
#  "secret": "435726770e17089f6c0b647f5ce7418ba6d07ca6b8c15d0c42e2379d1a09b6cc"
#}

# Derive keypair from the secret
stever seed pubkey 435726770e17089f6c0b647f5ce7418ba6d07ca6b8c15d0c42e2379d1a09b6cc
#{
#  "public": "72e8cb80621c41a95da3a004139ceefa39e8709e7a8183ed9ad601ce9a13714d",
#  "secret": "435726770e17089f6c0b647f5ce7418ba6d07ca6b8c15d0c42e2379d1a09b6cc"
#}
```

### Contract interaction

```bash
# Compute account address and stateinit
stever contract stateinit < ./path/to/Contract.tvc
#{
#  "address": "0:1df86a0f06aec400d04719052e6a17dffadc09f915c5e35e959d37d59beb7ac3",
#  "tvc": "te6ccgICAQAAA...some long base64 encoded BOC...AxWw=="
#}

# Execute getters locally
stever contract call \
    getParticipantInfo \
    '{"addr":"0:2f61300e70e2cdb5f96d3d7a0d60c70dfa515f89c3d4926e958b5eb147977469"}' \
    --addr '0:5325f4965e6388f97ae2578c19e8ffbc080f29d2357c5712d2a21d640dc10fb7' \
    --abi ./path/to/Contract.abi.json
#{
#  "code": 0,
#  "output": {
#    "lockDonor": "0:0000000000000000000000000000000000000000000000000000000000000000",
#    "locks": [],
#    "reinvest": true,
#    "reward": "0",
#    "stakes": [],
#    "total": "0",
#    "vestingDonor": "0:0000000000000000000000000000000000000000000000000000000000000000",
#    "vestings": [],
#    "withdrawValue": "0"
#  }
#}

# and others
```

### Execute node commands

```bash
# Get config params
stever node getparam 14
#{
#  "block_id": "-1:8000000000000000:156446:e6a099e43ba0e2a9b7b0d1e9b5207cef4e0e54c1dc2ea8811f0877ad78516bc0:fdca14025ba3b16b4286a561b7ade73f3e26a0224e9492cefc77b83ed649f37d",
#  "value": {
#    "basechain_block_fee": "073b9aca00",
#    "basechain_block_fee_dec": "1000000000",
#    "masterchain_block_fee": "076553f100",
#    "masterchain_block_fee_dec": "1700000000"
#  }
#}

# Send message
stever node sendmessage < ./path/to/message.boc

# and others
```

---

<details><summary><b>All options</b></summary>
<p>

```
Usage: stever [--root <root>] <command> [<args>]

All-in-one node management tool with support for the upcoming stEVER

Options:
  --root            path to the root directory
  --help            display usage information

Commands:
  init              Prepares configs and binaries
  validator         Validation manager service
  contract          Contract interaction stuff
  exporter          Prometheus metrics exporter
  node              Raw node tools operations
  seed              Seed utils
```

</p>
</details>

## How it works

This tool is a replacement of `ever-node-tools` and contains all the necessary functionality to manage a node.
During initialization steps it prepares configs (at `$HOME/.stever` by default), downloads and builds the node,
and deploys necessery contracts (all this through a cli with convenient choices!).

After contracts configuration this tool manages validator wallet (which is [EVER Wallet contract](https://github.com/broxus/ever-wallet-contract))
and optionally a DePool (default v3 or stEVER variant);

The update logic is based on two `systemd` services:

- `ever-validator` - the node itself;
- `ever-validator-manager` - service wrapper around `stever validator` command;

It uses two protocols to communicate with the node - the first one is for the control server (`TCP ADNL`),
and the second is for other stuff (`UDP ADNL`, same as the protocol used by all nodes in the network).
