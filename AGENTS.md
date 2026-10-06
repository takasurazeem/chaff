# Working on Chaff

## The model server — never start one, on either machine

**Do not start `llama-server`, vLLM, Ollama, or any other inference server. Not on this Mac, and
not on the Linux box either.** Not to test a feature, not temporarily, not "just to check the
endpoint", not because a pass needs it. The answer is no, and it stays no.

**The user starts and stops their own servers.** A model holds the GPU and tens of gigabytes of
memory for as long as it runs, and whether that trade is worth making right now is their call
about their hardware — not a decision a build step gets to make on their behalf.

This Mac especially: it is where they work, and a resident model would take exactly the resource
a 50,000-photograph grid needs. This application exists partly *because* a webview was using
1.4 GB it should not have been.

### Checking whether one is up is fine

A probe starts nothing:

```bash
# The Linux box, where models actually run when the user has one up.
curl -s -m 5 -o /dev/null -w "%{http_code}\n" http://192.168.1.150:8080/health

# And what is running over there, read-only.
ssh 192.168.1.150 'pgrep -c llama-server; ss -ltn | grep -c :8080'
```

`200` means tagging can use a vision model. Anything else means the **CLIP fallback on this
machine** — a different tagger with a closed vocabulary, which is why `TagPassReport::used`
names which one ran.

### If a pass needs a model and none is up

**Say so and stop.** Do not start one, and do not quietly fall back and describe the result as
if the vision model produced it. "No endpoint is configured; this will use CLIP, which is a
different tagger" is the honest answer, and the user can then decide.

## Tagging runs on this machine — the server is off limits

**Do not use the Linux model server for anything.** Not for tagging, not for a test, not "just to
compare". It is the user's, it hosts a different model than it used to, and the application is not
to depend on it.

**The direction is on-device inference.** The app ships its own small model and tags photographs
without a network at all. iPhone does this for faces and it is the right shape for this too: a
culling tool that stops working when a machine on the LAN is asleep is one people stop trusting
with a library.

### The budget

**500–700 MB is the ceiling for a model the app hosts itself**, and smaller is better. The current
one is **88 MB** — CLIP ViT-B/32 int8 — which is well inside it and already works.

| model | size | what it does |
|---|---|---|
| YuNet | 233 KB | face detection |
| CLIP vocabulary | 78 KB | the phrases CLIP chooses between |
| SFace | 38 MB | face recognition — grouping, not naming |
| **CLIP ViT-B/32 int8** | **88 MB** | **tagging, entirely on this machine** |

### What that means for a change

- **Prefer a better vocabulary over a bigger model.** CLIP's limitation is the 38 phrases it
  chooses between, not the encoder — the same weights with 400 phrases is a different product at
  no download cost.
- **A model above the ceiling needs asking first.** Do not quietly fetch something that takes a
  gigabyte of someone's disk because it scores better.
- **Never fall back to a server.** If the on-device path cannot do something, say so. Substituting
  a remote model and describing the result as if the local one produced it is the failure mode
  this section exists to prevent.

## Manual QA — the user drives, always

Build, install, launch, and read logs. Nothing else. Never navigate, click, scroll or type into
either app to decide whether something works, and **never write "verified" about UI the user has
not run**. A programmatic harness is a debugging aid, not QA, and it does not get committed.

Four bugs in one session were found by the user *looking* — including two where a fix was
reported as done without checking it reached anything.
