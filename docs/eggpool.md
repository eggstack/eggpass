# EggPool reverse tunnel

EggPool is an optional outbound reverse tunnel for an EggPass instance. EggPass binds its HTTP API only to a loopback address while EggPool is enabled. The reverse client uses a fixed target host and port taken from that loopback listener; neither the ticket nor the EggPool peer can select an arbitrary forwarding target.

## Setup

    eggpass eggpool --control pool.example:18443 --server-name pool.example --ca ./eggpool-ca.pem --label workstation

The command creates an EggPass API key for the agent, generates a random 256-bit enrollment token with a 10-minute lifetime, writes private state, enables the EggPool settings, and prints one single-line epagent1. ticket. If the daemon is already running, setup restarts it on the same loopback address to load the settings. If stopped, setup leaves it stopped.

The ticket is the prefix epagent1. followed by URL-safe, unpadded Base64 of UTF-8 JSON:

| Field | Meaning |
| --- | --- |
| v | Ticket format version (1). |
| control | Eggress control HOST:PORT. |
| server_name | TLS server name to verify. |
| ca_sha256 | URL-safe, unpadded Base64 SHA-256 pin of the first CA certificate DER bytes. The PEM bundle itself stays local. |
| enroll | One-time random bearer token, expiring at expires_at. |
| expires_at | Unix timestamp in seconds. |
| agent | Display label. |
| eggpass | EggPass package version. |
| api_key | EggPass API key for normal EggPass API calls; keys.json stores only its hash. |

Treat the ticket as a bearer secret. Do not paste it into issue trackers, logs, chat, telemetry, or diagnostics. It is printed to stdout only once. Provider OAuth access, refresh, and ID tokens are never part of the ticket or EggPool protocol.

## Enrollment exchange contract

The EggPool side must support one-time username/password authentication for the initial Eggress reverse connection: username enroll and password ticket.enroll. The connection must validate the TLS server with the ticket's CA and server_name. The first forwarded request must be:

    POST /v1/eggpool/enroll HTTP/1.1
    Authorization: Bearer <ticket.enroll>
    Content-Type: application/json

The JSON body must contain exactly these fields:

    {
      "client_cert_pem": "-----BEGIN CERTIFICATE-----...",
      "client_key_pem": "-----BEGIN PRIVATE KEY-----..."
    }

The client certificate and unencrypted private key must be a valid pair and fit within the size limits (64 KiB each). EggPass accepts only the exact callback path, compares the token in constant time, checks expiry, validates the TLS material, writes it to owner-only private state, and consumes the token only after successful validation and persistence. Repeated or expired enrollment cannot replace the certificate. The EggPass API key is not enrollment authorization.

After the exchange, EggPass reconnects using mutual TLS with the issued client certificate and key; the one-time password is removed from the reverse-client configuration. EggPool must authorize that certificate for the enrolled agent and route forwarded HTTP requests to the EggPass listener. The api_key field is for normal EggPass API requests and should be sent as Authorization: Bearer <api_key>; it is independent of mTLS and revocable locally.

### HTTP forwarding contract

Eggress relays an opaque TCP stream and does not rewrite HTTP headers. The EggPass loopback authority guard rejects the public EggPool hostname with HTTP 421. Before forwarding each HTTP request through the tunnel—including the enrollment callback—EggPool must rewrite the Host header to 127.0.0.1 (localhost is also accepted). Keep the TCP destination fixed to the configured EggPass loopback listener; do not use a caller-supplied Host or target to select a destination. The proxy must preserve the request path, method, body, and Authorization header.

Coordination requirement: this EggPass implementation defines the contract above. EggPool must implement the corresponding ticket parser, temporary initial auth, enrollment callback, client-certificate issuance, and subsequent mTLS acceptance before the full workflow can operate end to end.

## Status and revocation

eggpass status shows only a connection state, label, and control endpoint; it never prints ticket or certificate contents. Status states include starting, reconnecting, connected, enrollment_required, enrollment_expired, disconnected, error, and stopped. Status is local and may become stale if the daemon is killed without graceful shutdown.

    eggpass eggpool --revoke

Revocation stops a running daemon before removing the local EggPool API key, pending or enrolled secret material, and status file. It restarts only if another EggPass API key remains; otherwise the daemon stays stopped. It cannot revoke a certificate already issued at the EggPool server; EggPool must separately revoke that certificate or agent identity. Re-setup issues a new API key and enrollment token.
