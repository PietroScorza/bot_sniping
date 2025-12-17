# Solana Copytrading Bot 🤖

A high-performance copytrading bot for Solana built in Rust, using **Helius Yellowstone gRPC** for real-time transaction monitoring and **Jito bundles** for MEV-protected execution.

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Solana Copytrading Bot                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐       │
│  │   Helius     │───▶│   Trading    │───▶│    Jito      │       │
│  │  gRPC Client │    │    Engine    │    │   Client     │       │
│  └──────────────┘    └──────────────┘    └──────────────┘       │
│         │                   │                   │                │
│         ▼                   ▼                   ▼                │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐       │
│  │  Transaction │    │    State     │    │   Bundle     │       │
│  │   Decoder    │    │   Manager    │    │   Builder    │       │
│  └──────────────┘    └──────────────┘    └──────────────┘       │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## ✨ Features

- **Real-time monitoring** via Helius Yellowstone gRPC (sub-second latency)
- **Multi-DEX support**: Raydium, Jupiter, Pump.fun, Orca Whirlpool
- **First-buy-only logic**: Only copies the initial buy of a token
- **Dual selling strategy**:
  - Take Profit (TP) with configurable tiers
  - Emergency Copy-Sell with aggressive Jito tips
- **MEV protection** via Jito bundle submission
- **Automatic reconnection** with exponential backoff
- **In-memory state management** with trade history tracking

## 📋 Prerequisites

- Rust 1.75+ (Edition 2021)
- [Helius Premium account](https://helius.dev) with gRPC access
- Solana wallet with SOL for trading
- Access to Jito Block Engine

## 🚀 Quick Start

### 1. Clone and Setup

```bash
cd sniping_copytrading_v2
cp .env.example .env
```

### 2. Configure Environment

Edit `.env` with your credentials:

```env
# Your wallet private key (Base58 encoded)
PRIVATE_KEY=your_private_key_here

# Wallet to copy trades from
TARGET_WALLET=target_wallet_pubkey

# Helius credentials
HELIUS_GRPC_URL=https://atlas-mainnet.helius-rpc.com
HELIUS_API_KEY=your_helius_api_key

# Jito configuration
JITO_BLOCK_ENGINE_URL=https://frankfurt.mainnet.block-engine.jito.wtf
TIP_AMOUNT_NORMAL=10000
TIP_AMOUNT_EMERGENCY=100000

# Trading parameters
BUY_AMOUNT_SOL=0.1
SLIPPAGE_BPS=500
```

### 3. Build and Run

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Run
cargo run --release
```

## 📁 Project Structure

```
src/
├── main.rs              # Entry point, logging setup
├── config.rs            # Configuration from environment
├── grpc/
│   ├── mod.rs
│   └── helius_client.rs # Helius Yellowstone gRPC client
├── decoder/
│   ├── mod.rs
│   ├── parser.rs        # Transaction parsing logic
│   └── dex.rs           # DEX-specific instruction decoders
├── jito/
│   ├── mod.rs
│   ├── client.rs        # Jito Block Engine HTTP client
│   ├── bundle.rs        # Bundle construction
│   └── tip.rs           # Tip account management
├── state/
│   ├── mod.rs
│   ├── position.rs      # Position tracking structures
│   └── manager.rs       # State management (thread-safe)
└── engine/
    ├── mod.rs
    ├── core.rs          # Main trading engine logic
    └── executor.rs      # Trade execution (swap building)
```

## ⚙️ Configuration Options

### Trading Configuration

| Variable | Description | Default |
|----------|-------------|---------|
| `BUY_AMOUNT_SOL` | Fixed buy amount in SOL | `0.1` |
| `MAX_BUY_AMOUNT_SOL` | Maximum buy amount (safety cap) | `1.0` |
| `SLIPPAGE_BPS` | Slippage tolerance (basis points) | `500` |

### Take Profit Tiers

Configure multiple TP levels in JSON format:

```env
TAKE_PROFIT_TIERS=[{"multiplier":2.0,"sell_percent":20},{"multiplier":3.0,"sell_percent":30}]
```

### Jito Tips

| Variable | Description | Default |
|----------|-------------|---------|
| `TIP_AMOUNT_NORMAL` | Normal tip (lamports) for TP sells | `10000` |
| `TIP_AMOUNT_EMERGENCY` | Emergency tip for copy-sells | `100000` |
| `TIP_AMOUNT_MAX` | Maximum tip (safety cap) | `500000` |

## 🔧 Development

### Running Tests

```bash
cargo test
```

### Logging

Control log levels via `RUST_LOG`:

```bash
# Debug all bot logs
RUST_LOG=debug cargo run

# Info level with specific module debug
RUST_LOG=info,solana_copytrading_bot::engine=debug cargo run
```

## ⚠️ Important Notes

1. **Security**: Never commit your `.env` file or private keys
2. **Testing**: Test on devnet first before mainnet
3. **Capital**: Only trade with funds you can afford to lose
4. **Monitoring**: Watch for failed transactions and adjust tips accordingly

## 🔜 TODO / Known Limitations

- [ ] Complete DEX instruction builders (currently placeholders)
- [ ] Jupiter API integration for routing
- [ ] Redis state persistence
- [ ] Price monitoring for Take Profit
- [ ] Telegram/Discord notifications
- [ ] Web dashboard

## 📄 License

MIT License - See [LICENSE](LICENSE) for details.

## ⚠️ Disclaimer

This software is provided "as is" without warranty. Trading cryptocurrencies involves substantial risk of loss. This bot is for educational purposes only. Use at your own risk.
