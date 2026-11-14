# Client setup

`eggpass` speaks the OpenAI API. Any client that can talk to OpenAI can talk
to `eggpass`.

| Setting | Value |
| --- | --- |
| Base URL | `http://127.0.0.1:8484/v1` |
| API key | anything, e.g. `sk-local` — unless you issued a key, then that key |
| Model | any name from `GET /v1/models` |

Both `/v1/chat/completions` and `/chat/completions` work, so it does not
matter whether your client's base URL already carries the `/v1`.

If you issued a key with `eggpass key add`, the client **must** send it, as
`Authorization: Bearer <key>` or `X-Api-Key: <key>`.

---

## Finding the model names

The catalogue is fetched live from every provider you are signed in to, and it
is the union of them. There is no compiled-in list.

```sh
curl http://127.0.0.1:8484/v1/models
```

A model no provider advertises is a `404`, not a hopeful `200`. `eggpass
status` prints the count too.

Each entry carries what the provider says about the model — the thinking
levels it offers under `supported_reasoning_levels`, its context window, and
whether it takes images — so a client that reads the catalogue can tell the
user what the model does.

The levels are also offered under the names the wider OpenAI-compatible
ecosystem reads, so a client that looks for `capabilities.reasoning.effort`
finds them there too:

```json
{
  "id": "gpt-6.1-sol",
  "reasoning": true,
  "capabilities": { "reasoning": { "supported": true, "effort": ["low", "medium", "high", "xhigh", "max", "ultra"] } },
  "default_reasoning_level": "low",
  "limit": { "context": 500000 },
  "supported_reasoning_levels": [{ "effort": "low", "description": "Fast responses with lighter reasoning" }]
}
```

What a model does not publish is not guessed at: a model whose provider says
nothing about thinking carries no reasoning flag.

## Thinking levels

A thinking level is how much reasoning a model does before it answers. Set it
the way the Chat Completions API spells it, and the proxy translates it for
ChatGPT:

```python
answer = client.chat.completions.create(
    model="gpt-6.1-sol",
    messages=[{"role": "user", "content": "Hello"}],
    reasoning_effort="high",
)
print(answer.choices[0].message.reasoning_content)
```

The thinking comes back in `reasoning_content`, the field x.ai's answer uses
too, so one client shows it for both providers. In a streamed answer it
arrives in the same field, ahead of the answer itself.

x.ai always returns its thinking. The ChatGPT route writes a summary when it
chooses to, so `reasoning_content` is there when there is thinking to show and
absent when there is not. The level is applied either way — the model is
always asked to think at the level that was set.

Not every model takes every level — the levels a model offers are listed in
the catalogue — and a level it does not take is refused by the provider, with
the provider's own error.

---

## Tool calls

Tools work the same way for both providers: define them as the Chat
Completions API defines them, and the answer carries the call in
`message.tool_calls`.

```python
answer = client.chat.completions.create(
    model="gpt-6.1-sol",
    messages=[{"role": "user", "content": "What is the weather in Paris?"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
        },
    }],
)
call = answer.choices[0].message.tool_calls[0]
```

Send the result back in the same turn, with the call's own id, so the model can
carry on:

```python
answer = client.chat.completions.create(
    model="gpt-6.1-sol",
    messages=[
        {"role": "user", "content": "What is the weather in Paris?"},
        {"role": "assistant", "content": "", "tool_calls": answer.choices[0].message.tool_calls},
        {"role": "tool", "tool_call_id": call.id, "content": "18C, sunny"},
    ],
    tools=[...],
)
```

---

## OpenAI SDK

### Python

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8484/v1", api_key="not-needed")

stream = client.chat.completions.create(
    model="grok-4.3",
    messages=[{"role": "user", "content": "Hello"}],
    stream=True,
)
for chunk in stream:
    print(chunk.choices[0].delta.content or "", end="", flush=True)
```

With a key issued by `eggpass`, pass it instead:

```python
client = OpenAI(base_url="http://127.0.0.1:8484/v1", api_key="sk-eggpass-…")
```

### TypeScript

```ts
import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "http://127.0.0.1:8484/v1",
  apiKey: "not-needed",
});

const stream = await client.chat.completions.create({
  model: "gpt-5.6-sol",
  messages: [{ role: "user", content: "Hello" }],
  stream: true,
});

for await (const chunk of stream) {
  process.stdout.write(chunk.choices[0]?.delta?.content ?? "");
}
```

### The Responses API

`POST /v1/responses` is served natively for providers that speak it, so a
Responses-shaped client works without translation.

```sh
curl http://127.0.0.1:8484/v1/responses \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-5.6-sol","input":"Hello"}'
```

---

## OpenCode

Add the provider to `~/.config/opencode/opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "eggpass": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Grok and ChatGPT (local proxy)",
      "options": { "baseURL": "http://127.0.0.1:8484/v1" },
      "models": {
        "grok-4.3": { "name": "Grok 4.3" },
        "gpt-5": { "name": "GPT-5" }
      }
    }
  }
}
```

The model list in the config is yours to curate — `GET /v1/models` is the
live truth if you want the full set.

---

## Cline, Continue, and anything OpenAI-shaped

| Setting | Value |
| --- | --- |
| API provider | OpenAI Compatible |
| Base URL | `http://127.0.0.1:8484/v1` |
| API key | anything, e.g. `sk-local` — unless you have issued a key with `eggpass key add`, in which case use that one |
| Model | any model name from `GET /v1/models` |

---

## curl

```sh
curl http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"grok-4.3","messages":[{"role":"user","content":"Hello"}]}'
```

With a key:

```sh
curl http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer sk-eggpass-…' \
  -d '{"model":"grok-4.3","messages":[{"role":"user","content":"Hello"}]}'
```

### Streaming

Add `"stream": true` and use `curl -N` so the terminal is not buffering:

```sh
curl -N http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"grok-4.3","messages":[{"role":"user","content":"Hello"}],"stream":true}'
```

Each chunk is flushed as it arrives, so the first token shows up when the
model produces it.

---

## Health checks

`GET /health` is a liveness probe and is **never** authenticated, so it can
be used as-is by a monitor:

```sh
curl http://127.0.0.1:8484/health
```
