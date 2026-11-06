// API client: session gate → cookie+CSRF; all receipts share the core contract.
export let CSRF = "";

export function setCsrf(t: string) {
  CSRF = t;
}

export class ApiError extends Error {
  status: number;
  code: string;
  payload: any;
  constructor(status: number, payload: any) {
    super(payload?.error?.message || `HTTP ${status}`);
    this.status = status;
    this.code = payload?.error?.code || "error";
    this.payload = payload;
  }
}

async function call(method: string, path: string, body?: unknown, idem?: string): Promise<any> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (method !== "GET" && CSRF) headers["X-CSRF-Token"] = CSRF;
  if (idem) headers["Idempotency-Key"] = idem;
  const resp = await fetch(path, {
    method,
    headers,
    credentials: "same-origin",
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await resp.text();
  const json = text ? JSON.parse(text) : null;
  if (!resp.ok) throw new ApiError(resp.status, json);
  return json;
}

export const api = {
  health: () => call("GET", "/v1/health"),
  session: (code: string) => call("POST", "/v1/session", { code }),
  csrf: () => call("GET", "/v1/session/csrf"),
  doctor: () => call("GET", "/v1/doctor"),
  tasks: () => call("GET", "/v1/tasks"),
  task: (id: string) => call("GET", `/v1/tasks/${id}`),
  submit: (body: any, idem: string) => call("POST", "/v1/tasks", body, idem),
  trust: (id: string) => call("POST", `/v1/tasks/${id}/trust`, {}),
  plan: (id: string, writer?: string) => call("POST", `/v1/tasks/${id}/plan`, writer ? { writer_adapter: writer } : {}),
  approve: (id: string, hash: string) => call("POST", `/v1/tasks/${id}/plan/approve`, { summary_hash: hash }),
  routePreview: (id: string, role: string) => call("GET", `/v1/tasks/${id}/route-preview?role=${role}`),
  dispatch: (id: string, idem: string) => call("POST", `/v1/tasks/${id}/dispatch`, {}, idem),
  handoffPreview: (id: string, role: string) => call("GET", `/v1/tasks/${id}/handoff-preview?role=${role}`),
  diff: (id: string) => call("GET", `/v1/tasks/${id}/diff`),
  cancel: (attempt: string, idem: string) => call("POST", `/v1/attempts/${attempt}/cancel`, {}, idem),
  decisions: (open: boolean) => call("GET", `/v1/decisions?status=${open ? "open" : "all"}`),
  answer: (decision: string, option: string, revision: number, idem: string) =>
    call("POST", `/v1/decisions/${decision}/answer`, { option_id: option, revision }, idem),
  reroute: (id: string, target: string, idem: string) => call("POST", `/v1/tasks/${id}/reroute`, { target_adapter: target }, idem),
  accept: (id: string, candidate: string, idem: string) => call("POST", `/v1/tasks/${id}/accept`, { candidate_hash: candidate }, idem),
  resumeOptions: (id: string) => call("GET", `/v1/resume-options/${id}`),
  request: (id: string) => call("GET", `/v1/requests/${id}`),
};

export function randomIdem(prefix: string): string {
  const b = new Uint8Array(8);
  crypto.getRandomValues(b);
  return `${prefix}-${Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("")}`;
}
