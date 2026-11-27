# Client setup

`eggpass` exposes an OpenAI-compatible API at
`http://127.0.0.1:8484/v1`. Start the server, then configure your client with
the base URL, a client key, and a model ID from `GET /v1/models`.

```sh
eggpass login
eggpass key add laptop   # copy the key; it is shown once
eggpass run
export EGGPASS_API_KEY='paste-the-key-here'
```

The client must send the key as `Authorization: Bearer <key>` or `X-Api-Key:
<key>`. `eggpass` refuses to start without a configured key.

Both `/v1/chat/completions` and `/chat/completions` are accepted. Use the base
URL shown above for clients that expect the standard OpenAI path.

## Find model IDs

Fetch the live catalogue using your key:

```sh
curl http://127.0.0.1:8484/v1/models \
  -H "Authorization: Bearer $EGGPASS_API_KEY"
```

The response combines usable catalogues from signed-in providers. Provider
availability and normalization can affect which entries appear. Use an ID
from the response in your client configuration.

## OpenAI SDK

### Python

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8484/v1",
    api_key=os.environ["EGGPASS_API_KEY"],
)

answer = client.chat.completions.create(
    model="MODEL_ID_FROM_V1_MODELS",
    messages=[{"role": "user", "content": "Hello"}],
)
print(answer.choices[0].message.content)
```

For streaming, set `stream=True` and iterate over the returned chunks:

```python
stream = client.chat.completions.create(
    model="MODEL_ID_FROM_V1_MODELS",
    messages=[{"role": "user", "content": "Hello"}],
    stream=True,
)
for chunk in stream:
    print(chunk.choices[0].delta.content or "", end="", flush=True)
```

### TypeScript

```ts
import OpenAI from "openai";

const apiKey = process.env.EGGPASS_API_KEY;
if (!apiKey) throw new Error("Set EGGPASS_API_KEY first");

const client = new OpenAI({
  baseURL: "http://127.0.0.1:8484/v1",
  apiKey,
});

const answer = await client.chat.completions.create({
  model: "MODEL_ID_FROM_V1_MODELS",
  messages: [{ role: "user", content: "Hello" }],
});
console.log(answer.choices[0]?.message.content);
```

## OpenCode

Add an OpenAI-compatible provider to `~/.config/opencode/opencode.json`. Replace
the example model ID and API key with your values. Do not commit a config file
that contains a real key.

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "eggpass": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "eggpass",
      "options": {
        "baseURL": "http://127.0.0.1:8484/v1",
        "apiKey": "PASTE_KEY_FROM_EGGPASS_KEY_ADD"
      },
      "models": {
        "MODEL_ID_FROM_V1_MODELS": { "name": "Model name" }
      }
    }
  }
}
```

## Cline and Continue

Choose **OpenAI Compatible** as the provider and enter:

| Setting | Value |
| --- | --- |
| Base URL | `http://127.0.0.1:8484/v1` |
| API key | The key from `eggpass key add` |
| Model | An ID returned by `GET /v1/models` |

Other OpenAI-compatible clients use the same values.

## Responses API

`POST /v1/responses` is also available. Send the same client key:

```sh
curl http://127.0.0.1:8484/v1/responses \
  -H "Authorization: Bearer $EGGPASS_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"MODEL_ID_FROM_V1_MODELS","input":"Hello"}'
```

## Thinking levels and tool calls

Discover per-model thinking levels with the authenticated `GET /v1/models`
request shown above. Eggpass keeps the provider's native metadata and adds a
common, additive projection when effort levels are published:

- ChatGPT native fields: `supported_reasoning_levels[].effort` and
  `default_reasoning_level`.
- Grok native fields: `capabilities.reasoning_effort` and
  `capabilities.default_reasoning_effort`.
- Eggpass's common fields: `reasoning.supported_efforts` and, only when the
  provider supplies a default, `reasoning.default_effort`.

Effort labels (including new labels) and their provider order are preserved.
The common object is a sourced projection, not a claim of full OpenRouter
metadata conformance. Eggpass does not infer `default_enabled` or `mandatory`;
those behavioral properties are omitted because the supported provider
catalogue fields do not establish them. The previous
`capabilities.reasoning` alias is retained when no provider field already
occupies that location.

For Chat Completions, send `reasoning_effort` using a level listed for that
model. For example:

```json
{"model":"MODEL_ID_FROM_V1_MODELS","messages":[{"role":"user","content":"Think carefully about this."}],"reasoning_effort":"high"}
```

ChatGPT requests are translated to the Responses API's `reasoning.effort`;
Grok receives the Chat Completions effort value unchanged. Eggpass does not
validate levels locally, so the upstream remains authoritative when a value is
unsupported. Generic OpenAI-compatible clients may ignore custom model
metadata or may not show a thinking-level selector; client UI support varies.
Returned reasoning may be available in `reasoning_content`, depending on the
provider and model.

Tool calls use the Chat Completions format. The client should send the
assistant's tool call back with the tool result and its `tool_call_id`, as
required by the OpenAI API.

## curl

```sh
curl -N http://127.0.0.1:8484/v1/chat/completions \
  -H "Authorization: Bearer $EGGPASS_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"MODEL_ID_FROM_V1_MODELS","messages":[{"role":"user","content":"Hello"}],"stream":true}'
```

`-N` disables curl's output buffering. `GET /health` is an unauthenticated
liveness check:

```sh
curl http://127.0.0.1:8484/health
```
