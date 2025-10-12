# Debug Logging for irontsc

## Quick Start

The application now supports debug logging using the `RUST_LOG` environment variable.

## Usage Examples

### Basic Debug Mode
Run with debug logs for all components:
```bash
RUST_LOG=debug ./target/debug/irontsc
```

### IronRDP Specific Debug
Show only IronRDP debug logs:
```bash
RUST_LOG=ironrdp=debug ./target/debug/irontsc
```

### Trace Level (Most Verbose)
Get maximum detail from IronRDP:
```bash
RUST_LOG=ironrdp=trace ./target/debug/irontsc
```

### Multiple Component Logging
Fine-tune logging for different components:
```bash
RUST_LOG=ironrdp=debug,ironrdp_connector=trace,ironrdp_tokio=debug ./target/debug/irontsc
```

### Focus on Specific Modules
Debug only the connector and CredSSP:
```bash
RUST_LOG=ironrdp_connector=trace,sspi=debug ./target/debug/irontsc
```

### Info Level (Default)
Only show important information (default if RUST_LOG not set):
```bash
RUST_LOG=info ./target/debug/irontsc
# or just:
./target/debug/irontsc
```

### Warning/Error Only
Minimal logging:
```bash
RUST_LOG=warn ./target/debug/irontsc
# or
RUST_LOG=error ./target/debug/irontsc
```

## Log Levels (from most to least verbose)
1. `trace` - Very detailed, shows every step
2. `debug` - Detailed debugging information
3. `info` - General informational messages (default)
4. `warn` - Warning messages only
5. `error` - Error messages only

## Useful Component Names

- `ironrdp` - All IronRDP components
- `ironrdp_connector` - Connection establishment
- `ironrdp_tokio` - Async I/O operations
- `ironrdp_tls` - TLS/SSL operations
- `sspi` - Authentication (NTLM/Kerberos/CredSSP)
- `irontsc` - This application's logs

## Debugging CredSSP Issues

For CredSSP/authentication debugging:
```bash
RUST_LOG=ironrdp_connector=trace,sspi=trace ./target/debug/irontsc
```

## Debugging Connection Issues

For network/connection debugging:
```bash
RUST_LOG=ironrdp_connector=debug,ironrdp_tokio=debug,ironrdp_tls=debug ./target/debug/irontsc
```

## Output Redirection

Save logs to a file:
```bash
RUST_LOG=debug ./target/debug/irontsc 2> debug.log
```

Show on screen AND save to file:
```bash
RUST_LOG=debug ./target/debug/irontsc 2>&1 | tee debug.log
```

## Tips

- Start with `RUST_LOG=debug` to get a good overview
- Use `trace` level only when you need extreme detail (very verbose)
- Target specific components to reduce noise
- Logs go to stderr by default, so use `2>` to redirect them
