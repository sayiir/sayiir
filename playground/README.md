# Sayiir Playground

Interactive code playground powered by [Codapi](https://codapi.org/). Users can run Sayiir snippets (Python, Node.js) directly in the browser.

Two entry points:

| Entry | URL | Editor | Use case |
|-------|-----|--------|----------|
| `playground/index.html` | standalone file | Monaco (CDN) | Quick local testing |
| `website/src/pages/playground.astro` | `/playground` on docs site | Monaco (npm) | Production, integrated with Starlight |
| `<RunCode>` component | inline in docs pages | CodeMirror | Inline snippets in guides |

## Architecture

```
Browser  ──POST /v1/exec──>  Codapi server  ──Docker──>  sandbox container
                                                          (sayiir pre-installed)
```

- **Codapi** is a lightweight code execution server that runs user code inside Docker containers
- **Sandboxes** are Docker images with Sayiir pre-installed (`sandboxes/sayiir-python/`, `sandboxes/sayiir-node/`)
- **Monaco** provides the editor with intellisense — type declarations live in `sayiir.d.ts`

## Type Declarations

`playground/sayiir.d.ts` is the **single source of truth** for Monaco intellisense. It is derived from the actual TypeScript source in `sayiir-nodejs/src/`.

- The docs playground (`playground.astro`) reads this file at build time via `readFileSync`
- The standalone playground (`index.html`) fetches it at runtime via `fetch('sayiir.d.ts')`

When the Sayiir API changes, update `sayiir.d.ts` to match. No other files need inlined type strings.

## Local Development

### Prerequisites

- Docker (or OrbStack / Podman)
- [Codapi server](https://github.com/nalgeon/codapi) binary

### 1. Download Codapi

```bash
# macOS (Apple Silicon)
curl -L -o codapi.tar.gz \
  https://github.com/nalgeon/codapi/releases/download/v0.13.0/codapi_0.13.0_darwin_arm64.tar.gz

# macOS (Intel)
curl -L -o codapi.tar.gz \
  https://github.com/nalgeon/codapi/releases/download/v0.13.0/codapi_0.13.0_darwin_amd64.tar.gz

# Linux (amd64)
curl -L -o codapi.tar.gz \
  https://github.com/nalgeon/codapi/releases/download/v0.13.0/codapi_0.13.0_linux_amd64.tar.gz

tar xzf codapi.tar.gz -C playground/
rm codapi.tar.gz
```

### 2. Build sandbox images

```bash
cd playground

# Python (includes Rust toolchain for building the native extension)
docker build -f sandboxes/sayiir-python/Dockerfile \
  -t codapi/sayiir-python:latest sandboxes/sayiir-python

# Node.js
docker build -f sandboxes/sayiir-node/Dockerfile \
  -t codapi/sayiir-node:latest sandboxes/sayiir-node
```

### 3. Start Codapi

```bash
cd playground
./codapi
# listening on port 1313
```

### 4. Verify sandboxes

```bash
# Python
curl -s -H "content-type: application/json" \
  -d '{"sandbox":"sayiir-python","command":"run","files":{"main.py":"from sayiir import task, Flow, run_workflow\n\n@task\ndef greet(name):\n    return f\"Hello {name}\"\n\nwf = Flow(\"test\").then(greet).build()\nprint(run_workflow(wf, \"World\"))"}}' \
  http://localhost:1313/v1/exec | python3 -m json.tool

# Node.js
curl -s -H "content-type: application/json" \
  -d '{"sandbox":"sayiir-node","command":"run","files":{"main.js":"const { task, flow, runWorkflow } = require(\"sayiir\");\nconst greet = task(\"greet\", (name) => `Hello ${name}`);\nconst wf = flow(\"test\").then(greet).build();\nrunWorkflow(wf, \"World\").then(console.log);"}}' \
  http://localhost:1313/v1/exec | python3 -m json.tool
```

Expected output: `{ "id": "...", "ok": true, "stdout": "Hello World\n", "stderr": "" }`

### 5. Test the docs playground

```bash
cd website
pnpm dev
# Open http://localhost:4321/playground
# Also check http://localhost:4321/getting-started/python/ for the inline RunCode component
```

## Debugging

### "Connection error" when clicking Run

Codapi server is not running or not reachable.

```bash
# Check if Codapi is listening
curl http://localhost:1313/v1/exec -d '{}' -H 'content-type: application/json'
# Should return a JSON error (not a connection refused)
```

### Sandbox image not found

```bash
# List available images
docker images | grep codapi

# Rebuild if missing
docker build -f sandboxes/sayiir-python/Dockerfile -t codapi/sayiir-python:latest sandboxes/sayiir-python
```

### Code runs but imports fail ("No module named sayiir")

The sandbox image is outdated. Rebuild it to pick up the latest Sayiir version:

```bash
docker build --no-cache -f sandboxes/sayiir-python/Dockerfile \
  -t codapi/sayiir-python:latest sandboxes/sayiir-python
```

### Monaco intellisense not working (Node.js tab)

Check that `sayiir.d.ts` is being loaded. In the standalone playground, open DevTools Network tab and confirm the `sayiir.d.ts` fetch succeeds. If serving from `file://`, you may need a local HTTP server:

```bash
cd playground
python3 -m http.server 8080
# Open http://localhost:8080/index.html
```

### Codapi timeout / nproc errors

Adjust limits in `codapi.json`:

```json
{
  "step": { "timeout": 10 },
  "box": { "memory": 128, "nproc": 128 }
}
```

## Production Deployment (VPS)

### Prerequisites

- A VPS running Ubuntu 22.04+
- SSH key access to the VPS
- Cloudflare managing DNS for `sayiir.dev` (proxied A record for TLS)
- Ansible installed locally (`pip install ansible` or `brew install ansible`)

### 1. Review variables

Defaults in `deploy/group_vars/codapi.yml`:

| Variable | Default | Purpose |
|----------|---------|---------|
| `codapi_version` | `0.13.0` | Codapi server version |
| `codapi_domain` | `play.sayiir.dev` | Domain for nginx `server_name` |
| `codapi_cors_origin` | `https://docs.sayiir.dev` | Allowed CORS origin |
| `codapi_port` | `1313` | Codapi listen port |

### 2. Deploy

```bash
cd playground/deploy

# Full setup (first time): Docker, nginx, Codapi, sandboxes, systemd
ansible-playbook -i inventory.ini playbook.yml
```

### 3. Verify

```bash
curl -s -H "content-type: application/json" \
  -d '{"sandbox":"sayiir-python","command":"run","files":{"main.py":"from sayiir import task\nprint(\"live!\")"}}' \
  https://play.sayiir.dev/v1/exec
```

The playbook already runs a smoke test at the end — if it finishes green, the server is working.

### Updating sandboxes

When a new Sayiir version is released, rebuild sandbox images and restart:

```bash
cd playground/deploy

# Sync configs + rebuild images + restart + smoke test
ansible-playbook -i inventory.ini playbook.yml --tags update

# Force rebuild images (--no-cache equivalent)
ansible-playbook -i inventory.ini playbook.yml --tags update -e force_rebuild=true
```

### What the playbook does

```
1. Install Docker, enable service
2. Create `codapi` system user (in docker group)
3. Download Codapi binary to /opt/codapi/
4. Sync sandbox configs (Dockerfiles, commands.json, box.json)
5. Template codapi.json (production settings)
6. Build sayiir-python + sayiir-node Docker images
7. Install codapi systemd unit (starts on boot, restarts on failure)
8. Install nginx, template site config (reverse proxy + CORS)
9. Smoke test: POST a Python snippet, assert success
```

All tasks are idempotent — safe to re-run at any time.

### Tags

| Tag | Scope |
|-----|-------|
| `setup` | Full first-time provisioning |
| `update` | Sync configs, rebuild images, restart, smoke test |
| `test` | Smoke test only |

### Production config tuning

Edit `deploy/templates/codapi.json.j2` (or override on the VPS at `/opt/codapi/codapi.json`):

| Setting | Default | Notes |
|---------|---------|-------|
| `pool_size` | 8 | Max concurrent executions (fine for 2 vCPU) |
| `box.memory` | 64 | MB per container (enough for Sayiir snippets) |
| `step.timeout` | 5 | Seconds before killing a run |
| `verbose` | false | Set to `true` for debugging |

### Monitoring

```bash
# On the VPS:
systemctl status codapi          # service status
journalctl -u codapi -f          # codapi logs
journalctl -u nginx -f           # nginx logs
docker ps                        # running sandbox containers (short-lived)
```
