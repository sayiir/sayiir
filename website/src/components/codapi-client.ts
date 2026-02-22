const CODAPI_URL = import.meta.env.PUBLIC_CODAPI_URL || "http://localhost:1313";

export interface LangConfig {
  sandbox: string;
  entry: string;
}

export const LANGS: Record<string, LangConfig> = {
  python: { sandbox: "sayiir-python", entry: "main.py" },
  node: { sandbox: "sayiir-node", entry: "main.js" },
};

export interface RunResult {
  ok: boolean;
  stdout: string;
  stderr: string;
  duration: number;
}

export async function runCode(
  lang: string,
  code: string,
): Promise<RunResult> {
  const cfg = LANGS[lang];
  if (!cfg) throw new Error(`Unknown language: ${lang}`);

  // Detect ESM syntax for Node.js → use run-esm command + .mjs entry
  const isEsm = lang === "node" && /^\s*(import |export )/m.test(code);
  const entry = isEsm ? "main.mjs" : cfg.entry;
  const command = isEsm ? "run-esm" : "run";

  const start = performance.now();
  const res = await fetch(`${CODAPI_URL}/v1/exec`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      sandbox: cfg.sandbox,
      command,
      files: { [entry]: code },
    }),
  });

  const duration = Math.round(performance.now() - start);

  if (!res.ok) {
    const body = await res.text().catch(() => "");
    return {
      ok: false,
      stdout: "",
      stderr: `Server error ${res.status}${body ? `: ${body.slice(0, 200)}` : ""}`,
      duration,
    };
  }

  const data = await res.json();

  return {
    ok: data.ok,
    stdout: data.stdout || "",
    stderr: data.stderr || "",
    duration,
  };
}
