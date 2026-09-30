# ADR 0023: OpenAI-compatible chat model provider

Status: accepted for Task 019. Extends ADR 0008 (OpenAI Responses adapter)
without changing it.

## Context

The daemon could only run the closed OpenAI `gpt-5.6` Responses profile, so a
user needed that one paid account to use Ditto at all. Local servers (Ollama,
llama.cpp, vLLM, LM Studio) and most hosted providers (xAI, OpenRouter,
DeepSeek, Groq and others) expose the OpenAI-compatible `/chat/completions`
API. Supporting it lets the user pick cost, privacy and model freely,
including free local inference.

## Decision

`ditto-model-openai` gains `ChatCompletionsDriver`, a second driver in the same
crate that owns OpenAI wire formats:

- **Configuration.** The operator gives an API root and a model name
  (`--provider openai-compatible --base-url URL --model NAME`, or
  `DITTO_BASE_URL`/`DITTO_MODEL`). The endpoint is the root plus
  `/chat/completions`. HTTPS is required except for loopback hosts; URLs with
  credentials, queries or fragments are rejected, and redirects are never
  followed, so a key cannot leave the configured origin. An optional key comes only
  from `DITTO_MODEL_API_KEY` and is sent as a bearer token, never logged or
  echoed. Enabled runs still require a loopback listener. Usage reporting
  (`stream_options.include_usage`) is opt-in because some servers reject it.
- **Projection.** One system message holds the stable prefix and the context
  capsule, since many chat templates reject a second system message.
  Capability IDs become readable function names (`artifact.read` to
  `artifact_read`); valid IDs cannot collide, and IDs over 64 bytes use a
  digest. Tool calls, including text streamed before them, form one assistant
  message; tool results become `tool` messages. Structured output,
  provider continuation, explicit cache breakpoints and reasoning replay are
  rejected before any request is sent.
- **Streaming.** Only choice 0 is read. Text deltas stream as they arrive;
  tool calls may arrive split across chunks, whole, without IDs (IDs are then
  derived from the request ID), without indexes (a fragment joins the call
  with its ID, else a nameless fragment continues the latest call) or with
  object arguments. Unrequested usage is accepted. A finish reason
  readies every call and becomes the terminal at `[DONE]` or end of stream, so
  a trailing usage chunk still precedes it. `stop` with pending calls counts as
  tool calls. Reasoning deltas are ignored. Error chunks, invalid JSON,
  malformed arguments and a stream without a finish reason fail closed.
- **Lifecycle.** Both drivers now share one request lifecycle: cancellation and
  deadline checks, request and descriptor validation, compilation, bounded
  pre-response retry and controlled streaming. Nothing is retried after
  response headers.

## Rejected alternatives

- A separate crate would duplicate the SSE decoder, transport error handling
  and credential redaction.
- Hashed function names everywhere hide tool meaning from weaker models.
- Per-provider adapters would multiply code for one common wire format.
- Letting the model or a client choose the endpoint would create request
  forgery and credential routing risks; the operator fixes it at startup.

## Consequences and rollback

Any compatible server can drive runs, schedules and chats, including free local
models. Answer quality varies by model and stays unverified; tool use needs a
server and model with function calling. The closed Responses profile is
unchanged. Rollback removes the provider option and module; recorded turns are
provider-neutral and keep replaying.
