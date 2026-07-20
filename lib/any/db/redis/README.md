# RESP Client: Redis Library for Sond

A full RESP2 (REdis Serialization Protocol) encoder, parser, and TCP client library.

## Structure

| File | Description |
| -- | -- |
| `resp.snd` | RESP encoder/decoder (purity: pure, unit-testable) |
| `resp_test.snd` | Tests for resp.snd (25 tests, 100% coverage) |
| `client.snd` | TCP client with typed command helpers |
| `client_test.snd` | Compilation and encode-level tests |

## Commands covered

### String commands

`SET`, `GET`, `SETEX`, `GETSET`, `APPEND`, `STRLEN`, `INCR`, `INCRBY`, `DECR`, `DECRBY`

### Key commands

`DEL`, `EXISTS`, `EXPIRE`, `TTL`, `TYPE`, `KEYS`, `RENAME`, `FLUSHDB`, `DBSIZE`, `SELECT`

### List commands

`LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LLEN`, `LRANGE`, `LINDEX`, `LSET`, `LTRIM`, `LREM`

### Hash commands

`HSET`, `HGET`, `HDEL`, `HGETALL`, `HLEN`, `HKEYS`, `HVALS`, `HEXISTS`, `HINCRBY`

### Set commands

`SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SPOP`, `SRANDMEMBER`, `SDIFF`, `SINTER`, `SUNION`

### Sorted set commands

`ZADD`, `ZRANGE`, `ZSCORE`, `ZCARD`, `ZREM`, `ZRANK`, `ZREVRANK`, `ZINCRBY`, `ZCOUNT`,
`ZLEXCOUNT`, `ZREMRANGEBYRANK`, `ZREMRANGEBYSCORE`, `ZREMRANGEBYLEX`,
`ZRANGEBYSCORE`, `ZRANGEBYLEX`, `ZINTERSTORE`, `ZUNIONSTORE`, `ZMSCORE`

### Server commands

`PING`, `ECHO`, `INFO`, `CONFIG GET`, `SAVE`, `BGSAVE`, `LASTSAVE`, `SHUTDOWN`,
`DEBUG OBJECT`, `MONITOR`, `CLIENT LIST`, `CLIENT KILL`, `AUTH`, `QUIT`,
`TIME`, `UPTIME`, `SLAVEOF`, `WAIT`

### Transactions (MULTI/EXEC)

`MULTI`, `EXEC`, `DISCARD`, `WATCH`, `UNWATCH`

### Pub/Sub

`SUBSCRIBE`, `UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`

## API

### RESP encoding

```
resp_encode_command(n, items) -> i64
```
Encode a RESP command array. `n` is the argument count. `items` points to an i64 array
of `[n*2]` slots alternating `[addr, len]`. Returns a byte buffer with the encoded command.

```
enc_int(n) -> i64
enc_bulk_string(addr, len, b)
enc_nil() -> i64
enc_array_nil() -> i64
enc_array_of_strings(n, items) -> i64
```

### RESP parsing

```
resp_reset(data, len, start)
resp_decode() -> i64
resp_parse_int() -> i64
resp_parse_bulk() -> i64
resp_parse_simple() -> i64
```

### Client connection

```
redis_connect(host, port) -> fd
redis_close(fd)
redis_send(fd, buf, n) -> i64
redis_read(fd, buf, n) -> i64
```

Lower-level handle-based API:
```
redis_connect_new(port) -> i64   (handle)
redis_connect_close(handle)
redis_fill(handle) -> i64
redis_parse(handle) -> i64
redis_reset(handle)
```

### Sending commands

Every command helper follows the pattern:
```
redis_<cmd>(fd: i64, buf: i64, items: i64, ...) -> i64
```

- `fd`: socket file descriptor from `redis_connect`
- `buf`: output buffer handle (arena-allocated via heap.snd)
- `items`: scratch array `[max_args * 16]` for building the RESP argument list
- Additional parameters are key/value/integer arguments depending on the command

## Building and testing

```bash
# Compile the header (validates syntax)
sondc lib/any/db/redis/resp.snd --emit-header
sondc lib/any/db/redis/client.snd --emit-header

# Run unit tests
sondc lib/any/db/redis/resp_test.snd --test

# Execute tests
./resp_test
```

## RESP protocol

This library implements RESP2. Commands are sent as RESP arrays of bulk strings:

```
*<argc>\r\n
$<len1>\r\n<arg1>\r\n
$<len2>\r\n<arg2>\r\n
...
```

Replies use the five RESP types:
- `+` simple string
- `-` error
- `:` integer
- `$` bulk string (or `$-1` for nil)
- `*` array (or `*-1` for nil array)