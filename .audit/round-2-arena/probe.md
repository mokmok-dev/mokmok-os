# The provider wire shape, measured 2026-10-13

Three requests to `https://ollama.com/v1/chat/completions`, OpenAI-compatible, authenticated with
`Authorization: Bearer $OLLAMA_API_KEY`. The key is never in this file. The raw responses are beside
this note as `probe-tool-calls.json` and `probe-tool-result.json`; the streaming response is quoted
inline because its shape is the whole finding.

What the round needed to know is whether the tool protocol is structured or text, and whether a
full loop leg (call, tool result, answer) survives the wire. It is a fact, so it was measured rather
than assumed.

## 1. A structured tool call

```console
$ curl -s -H "Authorization: Bearer $OLLAMA_API_KEY" -H 'Content-Type: application/json' \
    -d '{"model":"kimi-k2.7-code","messages":[{"role":"system","content":"You are a coding agent…"},
      {"role":"user","content":"List the Rust source files…"}],
      "tools":[{"type":"function","function":{"name":"run_shell","description":"…",
      "parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}],
      "max_tokens":300,"stream":false}' \
    https://ollama.com/v1/chat/completions
http=200 time=1.436802s
```

The answer carries `choices[0].message.tool_calls[0].function.{name,arguments}` with
`arguments` a JSON **string**, `finish_reason: "tool_calls"`, `usage` with prompt and completion
token counts, and a non-standard `message.reasoning` string beside an empty `content`. So the model
answers with a structured call, and the vendor's `reasoning` field is a wire detail the adapter
discards rather than a domain field.

## 2. The tool-result leg closes the loop

The same request with the assistant's `tool_calls` echoed back and one `{"role":"tool",
"tool_call_id":…,"content":"./main.rs\n./lib.rs"}` answered `finish_reason: "stop"` with the final
prose in `content` (`probe-tool-result.json`, `http=200`, 1.83s).

So a turn is: ask with tools, receive a structured call, run it, hand the result back as a `tool`
message, receive either another call or the final answer. That is exactly the turn machine the
workspace already has (one `ModelAnswered` per turn, tools gated on it).

## 3. Streaming exists and is not needed

```console
$ curl -N … -d '{…,"stream":true,"max_tokens":40}' https://ollama.com/v1/chat/completions
data: {"id":"chatcmpl-145",…,"choices":[{"index":0,"delta":{"role":"assistant","content":"","reasoning":"We"},"finish_reason":null}]}
data: {"id":"chatcmpl-145",…,"choices":[{"index":0,"delta":{"content":"","reasoning":" need"},"finish_reason":null}]}
```

SSE chunks with `delta`. The durable record wants one complete answer per attempt, so the runtime
asks without streaming; a live tap is a UI concern for a later round.

## Also measured

`GET https://ollama.com/api/tags` lists `kimi-k2.7-code`, `kimi-k3`, `glm-5.3`, `glm-5.3-flash`,
`deepseek-v4.1-flash`, `deepseek-v4-pro:0813`, `minimax-m3`, `gpt-oss:120b`, `nemotron-3-ultra`, and
`mistral-large-4`. `https://api.x.ai/v1/models` answers `401`, so it is reachable with no key
presented.
