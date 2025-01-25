# Volt ⚡

A blazingly fast build cache server and CLI tool written in Rust. Volt helps you speed up your build times by caching and sharing build artifacts across your team.

## Features

- 🚀 Fast compression using zstd
- 🔒 Secure authentication
- 🔄 Simple push/pull cache operations
- 📦 Multi-directory caching support
- 🛠️ Build wrapper functionality

## Usage

Just type `volt` to generate the initial config, fill out the details, then create a new server using `volt server` and you are set!

- Manually push cache just
  `volt push`

- Manually pull cache just
  `volt pull`

## Architecture

Volt consists of two main components:

1. **Server** (`volt-server`): Handles cache storage and retrieval with authentication
2. **CLI** (`volt`): Manages cache operations and build wrapping

Built with 💜 using Rust
