# Working on Chaff

## The model server — never on this Mac

**Do not start `llama-server`, vLLM, Ollama, or any other local inference server on this
machine.** Not to test a feature, not temporarily, not "just to check the endpoint". The
answer is no, and it stays no.

The Mac is where the user works. A model server takes the GPU and tens of gigabytes of unified
memory, which is exactly the resource a 50,000-photograph grid needs — and this application
exists partly *because* a webview was using 1.4 GB it should not have been.

**Where models actually run:** the Linux box at `192.168.1.150:8080` (RTX 3090). That is a
different machine, reached over the network, and it is the user's to start and stop.

```bash
# Is it up? This is the only probe needed, and it starts nothing.
curl -s -m 5 -o /dev/null -w "%{http_code}\n" http://192.168.1.150:8080/health
```

`200` means tagging can use a vision model. Anything else means the **CLIP fallback on this
machine** — which is a different tagger with a closed vocabulary, and `TagPassReport::used` names
which one ran for exactly that reason.

## Manual QA — the user drives, always

Build, install, launch, and read logs. Nothing else. Never navigate, click, scroll or type into
either app to decide whether something works, and **never write "verified" about UI the user has
not run**. A programmatic harness is a debugging aid, not QA, and it does not get committed.

Four bugs in one session were found by the user *looking* — including two where a fix was
reported as done without checking it reached anything.
