import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { api, ApiError, randomIdem, setCsrf } from "./api";
import { Lang, t, statusText } from "./i18n";

// ---------- types ----------
type Task = any;
type Snapshot = any;

const FLIGHT_KINDS: Record<string, 0 | 1 | 2> = {
  attempt_started: 0,
  reroute_started: 1,
  result_collected: 2,
};

// ---------- motion stage (static wires + independent motion layer) ----------
function useMotion() {
  const layerRef = useRef<SVGGElement | null>(null);
  const timers = useRef<number[]>([]);
  const stop = useCallback(() => {
    timers.current.forEach((t) => window.clearTimeout(t));
    timers.current = [];
    if (layerRef.current) layerRef.current.replaceChildren();
  }, []);
  const play = useCallback(
    (kind: 0 | 1 | 2, fromHub: boolean) => {
      stop();
      if (document.hidden) return;
      if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
      const routes = [
        { path: "M 98 64 C 166 64 226 21 310 55 C 392 87 447 17 522 43", color: "#36a7c3", dur: 1.24 },
        { path: "M 522 43 C 447 17 392 87 310 55 C 382 85 449 114 522 83", color: "#e29c42", dur: 1.48 },
        { path: "M 522 83 C 449 114 382 85 310 55 C 226 21 166 64 98 64", color: "#51bd98", dur: 1.24 },
      ];
      const route = routes[kind];
      const NS = "http://www.w3.org/2000/svg";
      const layer = layerRef.current;
      if (!layer) return;
      const launchers: (() => void)[] = [];
      for (let i = 5; i >= 0; i--) {
        const delay = i * 0.043;
        const point = document.createElementNS(NS, "g");
        point.setAttribute("opacity", "0");
        if (i === 0) {
          const halo = document.createElementNS(NS, "circle");
          halo.setAttribute("r", "9");
          halo.setAttribute("fill", route.color);
          halo.setAttribute("filter", "url(#motion-halo)");
          const wing = document.createElementNS(NS, "path");
          wing.setAttribute("d", "M-1-2Q-7-8-3-8Q1-7 2-3M-1 2Q-7 8-3 8Q1 7 2 3");
          wing.setAttribute("fill", route.color);
          wing.setAttribute("opacity", ".55");
          const body = document.createElementNS(NS, "path");
          body.setAttribute("d", "M-6 0L-2-3.5H3L8 0L3 3.5H-2Z");
          body.setAttribute("fill", route.color);
          const eye = document.createElementNS(NS, "circle");
          eye.setAttribute("cx", "2");
          eye.setAttribute("r", "1.7");
          eye.setAttribute("fill", "#fff");
          point.append(halo, wing, body, eye);
        } else {
          const trail = document.createElementNS(NS, "ellipse");
          trail.setAttribute("rx", String(7 - i * 0.68));
          trail.setAttribute("ry", String(3 - i * 0.3));
          trail.setAttribute("fill", route.color);
          trail.setAttribute("opacity", String(1 - i * 0.12));
          point.append(trail);
        }
        const travel = document.createElementNS(NS, "animateMotion");
        travel.setAttribute("path", route.path);
        travel.setAttribute("rotate", "auto");
        travel.setAttribute("dur", `${route.dur}s`);
        travel.setAttribute("begin", "indefinite");
        travel.setAttribute("fill", "freeze");
        const fade = document.createElementNS(NS, "animate");
        fade.setAttribute("attributeName", "opacity");
        fade.setAttribute("values", "0;1;1;0");
        fade.setAttribute("keyTimes", "0;0.08;0.82;1");
        fade.setAttribute("dur", `${route.dur}s`);
        fade.setAttribute("begin", "indefinite");
        fade.setAttribute("fill", "freeze");
        point.append(travel, fade);
        layer.append(point);
        launchers.push(() => {
          travel.beginElementAt(delay);
          fade.beginElementAt(delay);
        });
      }
      launchers.forEach((l) => l());
      const cleanup = window.setTimeout(() => stop(), (route.dur + 0.3) * 1000);
      timers.current.push(cleanup);
    },
    [stop]
  );
  useEffect(() => {
    const onHide = () => {
      if (document.hidden) stop();
    };
    document.addEventListener("visibilitychange", onHide);
    return () => {
      document.removeEventListener("visibilitychange", onHide);
      stop();
    };
  }, [stop]);
  return { layerRef, play, stop };
}

function MotionStage({
  lang,
  phase,
  caption,
  receipt,
  onPlay,
  layerRef,
}: {
  lang: Lang;
  phase: 0 | 1 | 2;
  caption: string;
  receipt: { label: string; short: string } | null;
  onPlay: () => void;
  layerRef: React.RefObject<SVGGElement | null>;
}) {
  const outLabel =
    phase === 2 ? t("candidateSource", lang) : phase === 1 ? t("altRunner", lang) : phase === 0 ? t("runner", lang) : t("resultReturn", lang);
  return (
    <div className="motion-stage" data-phase={phase}>
      <div className="motion-bar">
        <span aria-live="polite">{caption}</span>
        <button onClick={onPlay}>{t("playRoute", lang)}</button>
      </div>
      <svg viewBox="0 0 620 118" aria-hidden="true" focusable="false">
        <defs>
          <filter id="motion-halo" x="-100%" y="-100%" width="300%" height="300%">
            <feGaussianBlur stdDeviation="3.5" />
          </filter>
        </defs>
        <path className="motion-wire" d="M 98 64 C 166 64 226 21 310 55 C 392 87 447 17 522 43" />
        <path className="motion-wire motion-branch" d="M 310 55 C 382 85 449 114 522 83" />
        <rect className="motion-station" x="92" y="58" width="12" height="12" rx="3" />
        <rect className="motion-station" x="303" y="48" width="14" height="14" rx="4" />
        <rect className="motion-station" x="516" y="37" width="12" height="12" rx="3" />
        <circle className="motion-station" cx="522" cy="83" r="5" />
        <g ref={layerRef} />
      </svg>
      <div className="motion-labels">
        <span>{t("taskEntrance", lang)}</span>
        <span>{t("hub", lang)}</span>
        <span>{outLabel}</span>
      </div>
      <div className="receipt-slot">
        {receipt && (
          <span className="route-receipt arrived">
            <span>{receipt.label}</span>
            <code>{receipt.short}</code>
          </span>
        )}
      </div>
    </div>
  );
}

// ---------- helpers ----------
function useSse(onEvent: (ev: any) => void) {
  const cbRef = useRef(onEvent);
  cbRef.current = onEvent;
  useEffect(() => {
    let es: EventSource | null = null;
    let lastSeq = 0;
    let closed = false;
    const connect = () => {
      if (closed) return;
      es = new EventSource("/v1/events");
      es.onmessage = (m) => {
        try {
          const data = JSON.parse(m.data);
          if (data.seq <= lastSeq) return; // dedupe by seq
          lastSeq = data.seq;
          cbRef.current(data);
        } catch {
          /* ignore */
        }
      };
      es.onerror = () => {
        es?.close();
        window.setTimeout(connect, 2000);
      };
    };
    connect();
    return () => {
      closed = true;
      es?.close();
    };
  }, []);
}

const pct = (a: any) => (a?.simulated ? "模拟 / simulated" : "真实 / real");

// ---------- app ----------
function App() {
  const [lang, setLang] = useState<Lang>((localStorage.getItem("wp_lang") as Lang) || "zh");
  const [theme, setTheme] = useState<string>(localStorage.getItem("wp_theme") || "light");
  const [authed, setAuthed] = useState(false);
  const [nav, setNav] = useState<"desk" | "tools" | "history">("desk");
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [inbox, setInbox] = useState<any[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [doctor, setDoctor] = useState<any>(null);
  const [route, setRoute] = useState<any>(null);
  const [receipt, setReceipt] = useState<{ label: string; short: string } | null>(null);
  const [flightPhase, setFlightPhase] = useState<0 | 1 | 2>(0);
  const [toast, setToast] = useState<string | null>(null);
  const [decisionChoice, setDecisionChoice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const { layerRef, play, stop } = useMotion();

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
    localStorage.setItem("wp_theme", theme);
    localStorage.setItem("wp_lang", lang);
  }, [theme, lang]);

  const showToast = useCallback((m: string) => {
    setToast(m);
    window.setTimeout(() => setToast(null), 3200);
  }, []);

  const refreshTasks = useCallback(async () => {
    try {
      const r = await api.tasks();
      setTasks(r.tasks);
      setInbox((r.inbox || []).filter((m: any) => !m.resolved));
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) setAuthed(false);
    }
  }, []);

  const refreshTask = useCallback(async (id: string) => {
    try {
      const s = await api.task(id);
      setSnap(s);
      return s;
    } catch {
      return null;
    }
  }, []);

  useEffect(() => {
    api
      .health()
      .then(async () => {
        try {
          const c = await api.csrf();
          setCsrf(c.csrf);
          setAuthed(true);
        } catch {
          setAuthed(false);
        }
      })
      .catch(() => setAuthed(false));
  }, []);

  useEffect(() => {
    if (!authed) return;
    refreshTasks();
    api.doctor().then(setDoctor).catch(() => {});
  }, [authed, refreshTasks]);

  const selectTask = useCallback(
    async (id: string | null) => {
      stop();
      setReceipt(null);
      setDecisionChoice(null);
      setSelected(id);
      setRoute(null);
      if (id) {
        const s = await refreshTask(id);
        if (s) {
          const cands = (s.candidates || []).filter((c: any) => c.current);
          if (s.task.status === "needs_review" && cands.length > 0) {
            setReceipt({ label: t("collected", lang), short: cands[0].candidate_hash.slice(0, 7) });
          }
          api.routePreview(id, "writer").then(setRoute).catch(() => {});
        }
      } else {
        setSnap(null);
      }
    },
    [lang, refreshTask, stop]
  );

  // SSE: refresh + real-event flight (dedupe by seq handled in useSse)
  useSse((ev) => {
    if (ev.task_id && ev.task_id === selected) {
      refreshTask(ev.task_id);
      const kind = FLIGHT_KINDS[ev.kind];
      if (kind !== undefined) {
        setFlightPhase(kind);
        play(kind, true);
        if (ev.kind === "result_collected") {
          const short = ev.payload?.candidate || ev.payload?.review;
          if (short) setReceipt({ label: t("collected", lang), short: String(short).slice(0, 7) });
        }
        if (ev.kind === "attempt_started" || ev.kind === "reroute_started") {
          setReceipt(null);
        }
      }
    }
    if (ev.kind === "inbox_updated" || ev.kind === "attempt_started" || ev.kind === "result_collected" || ev.kind === "candidate_accepted" || ev.kind === "task_blocked") {
      refreshTasks();
    }
  });

  // polls (fallback alongside SSE)
  useEffect(() => {
    if (!authed) return;
    const iv = window.setInterval(() => {
      refreshTasks();
      if (selected) refreshTask(selected);
    }, 5000);
    return () => window.clearInterval(iv);
  }, [authed, selected, refreshTasks, refreshTask]);

  if (!authed) {
    return <Gate onOk={async () => {
      const c = await api.csrf();
      setCsrf(c.csrf);
      setAuthed(true);
    }} lang={lang} />;
  }

  const openInbox = inbox.length;

  return (
    <div className="shell">
      <aside className="side">
        <div className="logo">
          <svg width="25" height="25" viewBox="0 0 25 25" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
            <path d="M10 11C4 9 5 3 9 4c3 1 4 4 4 6m1 1c2-6 8-7 8-3 0 3-4 5-7 5" />
            <path d="M3 17h5m-4 3h7" />
            <path d="M8 15l4-5 7 3-4 6-5-1z" fill="currentColor" />
            <circle cx="15" cy="14" r="1.2" fill="var(--side)" stroke="none" />
          </svg>
          <span>{t("brand", lang)}</span>
        </div>
        <nav className="nav" aria-label="main">
          <button data-nav="desk" aria-pressed={nav === "desk"} onClick={() => { setNav("desk"); setSelected(null); setSnap(null); }}>{t("desk", lang)}</button>
          <button data-nav="tools" aria-pressed={nav === "tools"} onClick={() => setNav("tools")}>{t("tools", lang)}</button>
          <button data-nav="history" aria-pressed={nav === "history"} onClick={() => setNav("history")}>{t("history", lang)}</button>
        </nav>
        <span className="local">{t("localWs", lang)}</span>
      </aside>
      <main>
        <header className="top">
          <div className="row">
            <strong>wagglepost</strong>
            <span className="muted">/</span>
            <code>v{("0.1.0")}</code>
          </div>
          <div className="top-controls">
            <button
              onClick={async () => {
                setNav("desk");
                setSelected("__new__");
              }}
              className="tag"
            >
              + {t("newTask", lang)}
            </button>
            {openInbox > 0 && (
              <button
                className="tag warn"
                aria-label={`${openInbox} ${t("needsAttention", lang)}`}
                onClick={() => {
                  setNav("desk");
                  const first = inbox[0];
                  if (first) selectTask(first.task_id);
                }}
              >
                {openInbox} {t("needsAttention", lang)}
              </button>
            )}
            <button id="language" onClick={() => setLang(lang === "zh" ? "en" : "zh")}>{lang === "zh" ? "EN" : "中文"}</button>
            <button onClick={() => setTheme(theme === "dark" ? "light" : "dark")} aria-label="toggle theme">◐</button>
          </div>
        </header>
        <div className="content">
          {nav === "desk" && (selected === "__new__" ? (
            <NewTask
              lang={lang}
              onDone={async (id) => {
                setSelected(null);
                showToast(t("submitOk", lang));
                await refreshTasks();
                await selectTask(id);
              }}
            />
          ) : selected ? (
            <TaskView
              key={selected}
              lang={lang}
              snap={snap}
              route={route}
              receipt={receipt}
              flightPhase={flightPhase}
              layerRef={layerRef}
              onPlay={() => {
                const kind = flightPhase;
                if (kind === 2 && !receipt) {
                  // static feedback under reduced motion
                }
                play(kind, true);
              }}
              decisionChoice={decisionChoice}
              setDecisionChoice={setDecisionChoice}
              busy={busy}
              setBusy={setBusy}
              showToast={showToast}
              refreshTask={refreshTask}
              refreshTasks={refreshTasks}
              selectTask={selectTask}
              onReceipt={(short) => setReceipt({ label: t("collected", lang), short })}
            />
          ) : (
            <DeskEmpty lang={lang} tasks={tasks} inbox={inbox} onSelect={selectTask} showToast={showToast} />
          ))}
          {nav === "tools" && <Tools lang={lang} doctor={doctor} onBack={() => setNav("desk")} />}
          {nav === "history" && <History lang={lang} tasks={tasks || []} onBack={() => setNav("desk")} onSelect={(id) => { setNav("desk"); selectTask(id); }} />}
        </div>
      </main>
      {toast && <div className="toast" role="status">{toast}</div>}
    </div>
  );
}

// ---------- gate ----------
function Gate({ onOk, lang }: { onOk: () => void; lang: Lang }) {
  const [code, setCode] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const submit = async () => {
    try {
      await api.session(code);
      onOk();
    } catch {
      setErr(t("sessionBad", lang));
    }
  };
  return (
    <div className="gate">
      <div className="panel">
        <span className="eyebrow">{t("brand", lang)}</span>
        <h2>{t("sessionGate", lang)}</h2>
        <input
          value={code}
          onChange={(e) => setCode(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && submit()}
          placeholder="startup code"
          autoFocus
        />
        {err && <p className="muted" role="alert">{err}</p>}
        <button className="primary" onClick={submit}>{t("sessionGo", lang)}</button>
      </div>
    </div>
  );
}

// ---------- desk empty / list ----------
function DeskEmpty({ lang, tasks, inbox, onSelect, showToast }: { lang: Lang; tasks: Task[] | null; inbox: any[]; onSelect: (id: string) => void; showToast: (m: string) => void }) {
  const list = tasks || [];
  return (
    <section className="panel">
      <div className="panel-head">
        <h2>{t("desk", lang)}</h2>
        <p>{list.length === 0 ? t("noTasks", lang) : ""}</p>
      </div>
      {list.length === 0 ? (
        <div className="empty">
          <p>{t("emptyDesk", lang)}</p>
          <button id="new-task-btn" className="primary" style={{ maxWidth: 280 }} onClick={() => onSelect("__new__")}>{t("newTask", lang)}</button>
          <div className="row" style={{ justifyContent: "center", flexWrap: "wrap" }}>
            <SampleButton lang={lang} kind="decision" onDone={onSelect} onError={(m) => showToast(m)} />
            <SampleButton lang={lang} kind="reroute" onDone={onSelect} onError={(m) => showToast(m)} />
          </div>
        </div>
      ) : (
        <div className="lineup tasklist">
          <div className="row" style={{ marginBottom: 6, flexWrap: "wrap" }}>
            <SampleButton lang={lang} kind="decision" onDone={onSelect} onError={(m) => showToast(m)} />
            <SampleButton lang={lang} kind="reroute" onDone={onSelect} onError={(m) => showToast(m)} />
          </div>
          {list.map((task) => (
            <button key={task.task_id} className="taskrow" onClick={() => onSelect(task.task_id)}>
              <span>
                <span className="t-title">{task.title}</span>
                <small>
                  {statusText(task.status, lang)} · {task.recipe === "write_then_review" ? (lang === "zh" ? "写后复核" : "write→review") : lang === "zh" ? "直接做" : "direct"}
                </small>
              </span>
              <span className={["needs_review", "waiting_decision", "quota_blocked", "interrupted", "stop_unconfirmed", "auth_required"].includes(task.status) ? "tag warn" : "tag"}>
                {statusText(task.status, lang)}
              </span>
            </button>
          ))}
        </div>
      )}
    </section>
  );
}

function SampleButton({ lang, onDone, onError, kind }: { lang: Lang; onDone: (id: string) => void; onError?: (m: string) => void; kind: "decision" | "reroute" }) {
  const [busy, setBusy] = useState(false);
  const submitSample = async () => {
    const r = await api.submit(
      {
        goal: kind === "decision"
          ? (lang === "zh" ? "在 README.md 末尾追加样例行。此样例演示：中断决策 → 预览正文 → 确认续答 → 复核接收（模拟）。" : "Append a sample line to README.md. Demonstrates: interruption decision -> preview body -> confirm resume -> review & accept (simulated).")
          : (lang === "zh" ? "在 README.md 末尾追加样例行。此样例演示：额度阻塞 → 保留成果 → 预览正文 → 确认换轨 → 复核接收（模拟）。" : "Append a sample line to README.md. Demonstrates: quota block -> keep changes -> preview body -> confirm reroute -> review & accept (simulated)."),
        repo_path: "sample:demo",
        recipe: "write_then_review",
        allowed_adapters: ["fake"],
        demo: kind,
        acceptance: [lang === "zh" ? "样例任务完整走完（模拟）" : "Sample task completes (simulated)"],
        ...(kind === "reroute" ? { resume_budget: 3 } : {}),
      },
      randomIdem("sample"),
    );
    const id = r.task.task_id;
    await api.trust(id);
    const p = await api.plan(id);
    await api.approve(id, p.plan.summary_hash);
    return id;
  };
  return (
    <button
      id={kind === "decision" ? "sample-btn" : "sample-btn-reroute"}
      className="tag"
      disabled={busy}
      onClick={async () => {
        setBusy(true);
        try {
          // WP13: the demo preset is pinned via `demo` in the submit body; no script
          // content crosses the API
          const id = await submitSample();
          onDone(id);
        } catch (e) {
          const msg = e instanceof ApiError ? `${e.code}: ${e.message}` : String(e);
          if (onError) onError(msg); else alert(msg);
        } finally {
          setBusy(false);
        }
      }}
    >
      {kind === "decision"
        ? (lang === "zh" ? "试用样例 · 决策续答" : "Sample · decision resume")
        : (lang === "zh" ? "试用样例 · 额度换轨" : "Sample · quota reroute")}
    </button>
  );
}

// ---------- new task ----------
function NewTask({ lang, onDone }: { lang: Lang; onDone: (id: string) => void }) {
  const [goal, setGoal] = useState("");
  const [repo, setRepo] = useState("");
  const [recipe, setRecipe] = useState("write_then_review");
  const [adapters, setAdapters] = useState("codex, claude");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  return (
    <section className="panel">
      <div className="panel-head">
        <h2>{t("newTask", lang)}</h2>
        <p>{lang === "zh" ? "提交后先看路线与原因，确认才派发。" : "Review the route and reasons before approving dispatch."}</p>
      </div>
      <div className="detail newtask">
        <label>
          {t("goalField", lang)}
          <textarea rows={4} value={goal} onChange={(e) => setGoal(e.target.value)} />
        </label>
        <label>
          {t("repoField", lang)}
          <input value={repo} onChange={(e) => setRepo(e.target.value)} placeholder="D:\path\to\repo" />
        </label>
        <label>
          {t("recipeField", lang)}
          <select value={recipe} onChange={(e) => setRecipe(e.target.value)}>
            <option value="write_then_review">{t("recipeWTR", lang)}</option>
            <option value="direct">{t("recipeDirect", lang)}</option>
          </select>
        </label>
        <label>
          {lang === "zh" ? "允许的运行时（逗号分隔）" : "Allowed runtimes (comma separated)"}
          <input value={adapters} onChange={(e) => setAdapters(e.target.value)} />
        </label>
        {err && <p className="muted" role="alert">{err}</p>}
        <button
          className="primary"
          disabled={busy || !goal.trim() || !repo.trim()}
          onClick={async () => {
            setBusy(true);
            setErr(null);
            try {
              const r = await api.submit(
                {
                  goal,
                  repo_path: repo,
                  recipe,
                  allowed_adapters: adapters.split(",").map((s) => s.trim()).filter(Boolean),
                },
                randomIdem("web-submit")
              );
              const id = r.task.task_id;
              try {
                await api.trust(id);
              } catch { /* shown later at approval */ }
              onDone(id);
            } catch (e) {
              setErr(e instanceof ApiError ? `${e.code}: ${e.message}` : String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {t("submit", lang)}
        </button>
      </div>
    </section>
  );
}

// ---------- task view ----------
function TaskView({
  lang,
  snap,
  route,
  receipt,
  flightPhase,
  layerRef,
  onPlay,
  decisionChoice,
  setDecisionChoice,
  busy,
  setBusy,
  showToast,
  refreshTask,
  refreshTasks,
  selectTask,
  onReceipt,
}: any) {
  const [resumePreview, setResumePreview] = useState<{ digest: string; text: string; bytes: number; decisionId: string; optionId: string } | null>(null);
  const [handoff, setHandoff] = useState<any>(null);
  const [diff, setDiff] = useState<any>(null);
  const [showDiff, setShowDiff] = useState(false);
  const [requestInfo, setRequestInfo] = useState<any>(null);

  useEffect(() => {
    if (!snap) return;
    const task = snap.task;
    const status = task.status;
    if (["awaiting_approval", "queued"].includes(status) && snap.plan) {
      api.handoffPreview(task.task_id, "writer").then(setHandoff).catch(() => setHandoff(null));
    }
  }, [snap?.task?.status, snap?.task?.task_id]);

  if (!snap) return <p className="muted">…</p>;
  const task = snap.task;
  const status = task.status;
  const langBtns = lang;
  const attempts: any[] = snap.attempts || [];
  const decisions: any[] = (snap.decisions || []).filter((d: any) => d.status === "open");
  const decision = decisions[0];
  const reviews: any[] = snap.reviews || [];
  const currentCand = (snap.candidates || []).find((c: any) => c.current);
  const simulated = attempts.length > 0 && attempts.every((a: any) => a.simulated);

  // the receipt stamp derives from snapshot state so it is readable before any
  // flight and always matches the candidate that was actually collected
  const derivedReceipt =
    currentCand && ["needs_review", "accepted"].includes(status)
      ? { label: t("collected", lang), short: String(currentCand.candidate_hash).slice(0, 7) }
      : receipt;
  const phase: 0 | 1 | 2 = ["quota_blocked", "auth_required", "interrupted", "cancelled", "stop_unconfirmed"].includes(status)
    ? 1
    : receipt
      ? 2
      : 0;

  const caption =
    phase === 1
      ? t("reroutePath", lang)
      : phase === 2
        ? t("returnPath", lang)
        : ["running", "starting", "stopping", "needs_input"].includes(status)
          ? t("dispatchPath", lang)
          : t("taskTrajectory", lang);

  // main action per contract: one bounded action from the receipt
  const next = snap.receipt?.next_action?.action;
  const primary = async () => {
    setBusy(true);
    try {
      if (status === "awaiting_approval" && snap.plan) {
        await api.approve(task.task_id, snap.plan.summary_hash);
        showToast(lang === "zh" ? "计划已确认，进入队列" : "Plan approved, queued");
      } else if (next === "dispatch" || status === "queued" || status === "needs_review" && attempts.length === 0) {
        const r = await api.dispatch(task.task_id, randomIdem("web-disp"));
        showToast(`${t("dispatched", lang)} · ${r.receipt?.attempt_id ?? ""}`);
      } else if (status === "waiting_decision" && decision && decisionChoice) {
        // WP09 two-step: the FIRST click on an option shows the exact body; only the
        // confirm click on a FRESH preview submits the digest. Option/task changes
        // revoke the pending preview.
        if (!resumePreview || resumePreview.decisionId !== decision.decision_id || resumePreview.optionId !== decisionChoice) {
          const prev = await api.handoffPreview(task.task_id, "writer", decision.decision_id, decisionChoice);
          setResumePreview({
            digest: prev.approved_digest,
            text: prev.handoff.text,
            bytes: prev.handoff.bytes,
            decisionId: decision.decision_id,
            optionId: decisionChoice,
          });
          setBusy(false);
          return;
        }
        const r = await api.answer(
          decision.decision_id,
          decisionChoice,
          task.revision,
          resumePreview.digest,
          randomIdem("web-ans"),
        );
        setResumePreview(null);
        showToast(t("decisionQueued", lang));
      } else if (next === "reconcile") {
        const info = await api.request(attempts[attempts.length - 1]?.request_id || task.task_id);
        setRequestInfo(info);
      } else if (next === "consider_reroute") {
        // reroute panel below offers candidates
      } else if (status === "needs_review" && currentCand) {
        const r = await api.accept(task.task_id, currentCand.candidate_hash, randomIdem("web-acc"));
        showToast(`${t("accepted", lang)} · ${String(r.candidate_hash).slice(0, 7)}`);
      }
      await refreshTask(task.task_id);
      refreshTasks();
    } catch (e) {
      showToast(e instanceof ApiError ? `${e.code}: ${e.message}` : String(e));
    } finally {
      setBusy(false);
    }
  };

  const primaryLabel = (() => {
    if (status === "awaiting_approval") return t("approvePlan", langBtns);
    if (next === "dispatch") return t("confirmDispatch", langBtns);
    if (status === "waiting_decision") {
      return resumePreview && decision && resumePreview.decisionId === decision.decision_id && resumePreview.optionId === decisionChoice
        ? (lang === "zh" ? "确认发送此正文" : "Confirm this body")
        : t("answerDecision", langBtns);
    }
    if (next === "reconcile") return t("checkOriginalRequest", langBtns);
    if (phase === 1) return t("confirmReroute", langBtns);
    if (status === "needs_review") return currentCand ? t("acceptCandidate", langBtns) : t("viewCandidate", lang);
    if (status === "accepted") return t("candidateAccepted", langBtns);
    return t("viewStatus", lang) || statusText(status, lang);
  })() as string;

  const primaryDisabled =
    busy ||
    (status === "waiting_decision" && !decisionChoice) ||
    status === "accepted" ||
    ["running", "starting", "stopping"].includes(status) ||
    (status === "needs_review" && !currentCand && attempts.length > 0);

  const facts: string[] = [];
  if (snap.plan) {
    facts.push(
      `${lang === "zh" ? "角色" : "Roles"}: ` +
        snap.plan.steps.map((s: any) => `${s.role === "writer" ? t("writerRole", lang) : t("reviewRole", lang)} · ${s.adapter}${s.permission === "read-only" ? " (read-only)" : ""}`).join(" → ")
    );
  }
  facts.push(`${t("launchBudget", lang)}: ${task.launches_used}/${task.planned_steps + task.resume_budget}`);
  if (currentCand) facts.push(`${t("candidate", lang)}: ${currentCand.candidate_hash.slice(0, 7)} (${currentCand.generation})`);
  const lastLive = attempts.find((a: any) => a.generation === task.current_generation && a.role === "writer");
  if (["interrupted", "quota_blocked", "cancelled"].includes(status) && lastLive) {
    facts.push(t("stopEvidence", lang));
  }

  return (
    <>
      <section className="intro">
        <span className="eyebrow">{lang === "zh" ? "任务" : "TASK"} {task.task_id.slice(-4)}</span>
        <h1>{task.title}</h1>
        <p>
          {task.goal.length > 120 ? task.goal.slice(0, 120) + "…" : task.goal}
        </p>
      </section>
      <div className="layout">
        <section className="panel">
          <div className="panel-head">
            <h2>{t("route", lang)}</h2>
            <p>
              {task.recipe === "write_then_review" ? (lang === "zh" ? "写后复核 · 同仓库单写者" : "Implement, then review · one writer") : lang === "zh" ? "直接做 · 单写者" : "Direct · single writer"}
              {simulated ? ` · ${t("simulated", lang)}` : ""}
            </p>
          </div>
          <MotionStage lang={lang} phase={phase} caption={caption} receipt={derivedReceipt} onPlay={onPlay} layerRef={layerRef} />
          <div className="lineup" id="lineup">
            {(snap.plan?.steps || []).map((s: any, i: number) => {
              const st = attempts.find((a: any) => a.step_id === s.step_id);
              const badge = st
                ? st.status === "completed"
                  ? lang === "zh"
                    ? "已完成"
                    : "Done"
                  : st.status === "running"
                    ? t("running", lang)
                    : statusText(status, lang)
                : lang === "zh"
                  ? "待执行"
                  : "Pending";
              const warnBadge = ["stopping", "interrupted", "quota_blocked", "waiting_decision", "stop_unconfirmed"].includes(status) && s.role === "writer";
              return (
                <button
                  key={s.step_id}
                  className="station"
                  onClick={async () => {
                    await api.routePreview(task.task_id, s.role).catch(() => null);
                  }}
                  aria-pressed={false}
                >
                  <span className="mark">{i + 1}</span>
                  <span>
                    {s.role === "writer" ? t("writerRole", lang) : t("reviewRole", lang)} · {s.adapter}
                    {st?.simulated ? ` (${t("simulated", lang)})` : ""}
                    <small>{st?.adapter_version || s.permission}</small>
                  </span>
                  <span className={"tag" + (warnBadge ? " warn" : "")}>{badge}</span>
                </button>
              );
            })}
          </div>
        </section>
        <section className="panel detail" aria-live="polite">
          <div>
            <span className={"tag" + (phase === 1 || status === "waiting_decision" ? " warn" : "")}>{statusText(status, lang)}</span>
            <h2>
              {status === "needs_review" && reviews.length > 0
                ? t("candidateReviewed", lang)
                : phase === 1
                  ? t("stopAndReroute", lang)
                  : status === "waiting_decision"
                    ? t("waitingDecision", lang)
                    : t("whyThisRoute", lang)}
            </h2>
          </div>
          <p className="muted">
            {phase === 1
              ? lang === "zh"
                ? "原写者已停止；已保留内容可带入下一次执行。"
                : "The previous writer stopped; kept changes can continue."
              : status === "waiting_decision"
                ? lang === "zh"
                  ? "执行槽已释放。回答后从检查点继续。"
                  : "Slot released. Answer to resume from the checkpoint."
                : lang === "zh"
                  ? "路线由本地规则得出；下方显示每个候选的原因。"
                  : "Routing is local rules; candidate reasons below."}
          </p>
          <ul id="facts">
            {facts.map((f, i) => (
              <li key={i}>{f}</li>
            ))}
          </ul>
          {resumePreview && status === "waiting_decision" && (
            <details open>
              <summary>
                {lang === "zh" ? "待发送正文预览" : "Body to be sent"} — {resumePreview.bytes} {t("bytes", lang)}
              </summary>
              <div className="diffbox">{resumePreview.text}</div>
              <p className="muted" style={{ fontSize: 12 }}>
                digest: <code>{resumePreview.digest}</code>
              </p>
            </details>
          )}
          {decision && (
            <fieldset className="decision-options">
              <legend>{t("chooseOption", lang)}</legend>
              {decision.options.map((o: any) => (
                <label key={o.id}>
                  <input
                    type="radio"
                    name="decision"
                    checked={decisionChoice === o.id}
                    onChange={() => setDecisionChoice(o.id)}
                  />
                  <span>
                    {(lang === "zh" ? o.label_zh : o.label_en) || o.id}
                    <small className="muted" style={{ display: "block" }}>{lang === "zh" ? o.effect_zh : o.effect_en}</small>
                  </span>
                </label>
              ))}
            </fieldset>
          )}
          {requestInfo && (
            <p className="muted">
              <code>{requestInfo.request?.request_id}</code> — {requestInfo.guidance}
            </p>
          )}
          {phase === 1 && (
            <ReroutePanel lang={lang} task={task} snap={snap} busy={busy} setBusy={setBusy} showToast={showToast} refreshTask={refreshTask} refreshTasks={refreshTasks} />
          )}
          {handoff && ["awaiting_approval"].includes(status) && (
            <details>
              <summary>
                {t("sendBody", lang)} — {handoff.handoff.bytes} {t("bytes", lang)}
              </summary>
              <div className="diffbox">{handoff.handoff.text}</div>
            </details>
          )}
          {currentCand && ["needs_review", "accepted"].includes(status) && (
            <details
              open={false}
              onToggle={async (e) => {
                if ((e.target as HTMLDetailsElement).open && !diff) {
                  const d = await api.diff(task.task_id).catch(() => null);
                  setDiff(d);
                }
              }}
            >
              <summary>
                {t("diffLabel", lang)} · {currentCand.candidate_hash.slice(0, 7)}
              </summary>
              {diff ? (
                <>
                  <div className="diffbox">{diff.diff || (lang === "zh" ? "（无差异文本）" : "(no diff text)")}</div>
                  <div className="row" style={{ marginTop: 8 }}>
                    <button
                      onClick={() => {
                        navigator.clipboard.writeText(diff.diff || "").then(
                          () => showToast(t("copyDiff", lang)),
                          () => showToast("clipboard failed")
                        );
                      }}
                    >
                      {t("copyDiff", lang)}
                    </button>
                  </div>
                </>
              ) : (
                <p className="muted">…</p>
              )}
            </details>
          )}
          {reviews.length > 0 && (
            <ul className="reasons">
              {reviews.map((r) => (
                <li key={r.review_id}>
                  <span className={r.historical ? "tag" : "tag warn"}>
                    {r.historical ? t("staleReview", lang) : `${t("reviewRole", lang)}: ${r.verdict}`}
                  </span>{" "}
                  <code>{r.candidate_hash.slice(0, 7)}</code> · {r.summary?.slice(0, 90)}
                </li>
              ))}
            </ul>
          )}
          <button className="primary" disabled={primaryDisabled} onClick={primary}>
            {status === "accepted" ? t("candidateAccepted", lang) : primaryLabel}
          </button>
          {simulated && (
            <span className="muted">
              {lang === "zh" ? "本任务的运行为模拟（fake 适配器）。" : "Runs in this task are simulated (fake adapter)."}
            </span>
          )}
        </section>
      </div>
      <details className="panel footer">
        <summary>{t("messagesArtifacts", lang)}</summary>
        <div className="reasons" style={{ marginTop: 10 }}>
          {(snap.events || [])
            .slice(-30)
            .reverse()
            .map((e: any) => (
              <p key={e.seq} className="muted" style={{ fontSize: 12 }}>
                <code>#{e.seq}</code> {e.kind} {JSON.stringify(e.payload).slice(0, 110)}
              </p>
            ))}
        </div>
      </details>
    </>
  );
}

function ReroutePanel({ lang, task, snap, busy, setBusy, showToast, refreshTask, refreshTasks }: any) {
  const [resume, setResume] = useState<any>(null);
  const [handoff, setHandoff] = useState<any>(null);
  const [targets, setTargets] = useState<any[]>([]);
  // WP11: per-target two-step confirm — click shows the exact body; the confirm
  // click submits THAT digest; changing target clears the pending confirmation
  const [pending, setPending] = useState<{ target: string; digest: string } | null>(null);
  useEffect(() => {
    api.resumeOptions(task.task_id).then(setResume).catch(() => {});
    api.routePreview(task.task_id, "writer").then((r) => setTargets(r.candidates || [])).catch(() => {});
    api.handoffPreview(task.task_id, "writer").then(setHandoff).catch(() => {});
  }, [task.task_id]);
  const cp = (snap.checkpoints || [])[0];
  const eligible = targets.filter((c) => c.ok && !c.simulated);
  const gateBlocked = targets.filter((c) => !c.ok && ["version_unavailable", "permission_insufficient", "role_write_not_supported"].some((rc) => (c.reasons || []).some((r: any) => r.reason_code === rc)));
  return (
    <div className="reasons">
      <p>
        <strong>{t("keptChanges", lang)}</strong>{" "}
        {lang === "zh" ? "检查点已保存" : "checkpoint saved"} {cp ? <code>{String(cp.checkpoint_id).slice(0, 10)}</code> : null}
      </p>
      <p>{t("stopEvidence", lang)}</p>
      <p>{t("extraBudget", lang)}</p>
      {resume && (
        <p className="muted">
          {resume.same_session_resume
            ? lang === "zh"
              ? "可确认同一原生会话：续跑只发增量。"
              : "Same native session confirmed: resume sends the delta only."
            : lang === "zh"
              ? "无法确认同一原生会话：续跑发送完整精简包（预览后确认）。"
              : "Same session unconfirmed: a full compact package needs preview + approval."}
        </p>
      )}
      {handoff && (
        <details open={!!pending}>
          <summary>
            {t("sendBody", lang)} — {handoff.handoff.bytes} {t("bytes", lang)}
          </summary>
          <div className="diffbox">{handoff.handoff.text}</div>
        </details>
      )}
      <details>
        <summary>{t("rerouteCandidates", lang)}</summary>
        <ul className="reasons">
          {targets.map((c) => (
            <li key={c.adapter}>
              <button
                className="tag"
                disabled={busy || !c.ok}
                onClick={async () => {
                  if (!pending || pending.target !== c.adapter) {
                    // first click: show the exact body for THIS target
                    const prev = await api.handoffPreview(task.task_id, "writer").catch(() => null);
                    if (prev) {
                      setPending({ target: c.adapter, digest: prev.approved_digest });
                      setHandoff(prev);
                    }
                    setBusy(false);
                    return;
                  }
                  setBusy(true);
                  try {
                    await api.reroute(task.task_id, c.adapter, pending.digest, randomIdem("web-rr"));
                    showToast(lang === "zh" ? "换轨已排队" : "Reroute queued");
                    setPending(null);
                    await refreshTask(task.task_id);
                    refreshTasks();
                  } catch (e) {
                    showToast(e instanceof ApiError ? `${e.code}: ${e.message}` : String(e));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                {pending && pending.target === c.adapter
                  ? `${c.adapter} → ${lang === "zh" ? "确认发送此正文" : "Confirm this body"}`
                  : `${c.adapter} → ${lang === "zh" ? "预览并换轨" : "Preview and reroute"}`}
              </button>{" "}
              {!c.ok && (
                <span className="muted">
                  {(c.reasons || []).map((r: any) => (lang === "zh" ? r.text_zh : r.text_en)).join("; ")}
                </span>
              )}
            </li>
          ))}
        </ul>
        {eligible.length === 0 && gateBlocked.length > 0 && (
          <p className="muted">
            {lang === "zh"
              ? "没有已通过写入门槛的第二运行时：可导出已生成产物后人工接手（下方导出）。"
              : "No second runtime has passed the write gate: export the produced work and take over manually (export below)."}
          </p>
        )}
      </details>

    </div>
  );
}

// ---------- tools ----------
function Tools({ lang, doctor, onBack }: { lang: Lang; doctor: any; onBack: () => void }) {
  const adapters = doctor?.adapters || [];
  return (
    <section className="panel">
      <div className="panel-head">
        <h2>{t("runtimeCaps", lang)}</h2>
        <p>{lang === "zh" ? "doctor 探测的本机能力；未过门槛的能力如实显示。" : "On-device capabilities from doctor; unpassed gates are shown honestly."}</p>
      </div>
      <div className="lineup tasklist">
        {adapters.map((a: any) => (
          <div key={a.id} className="taskrow" style={{ cursor: "default" }}>
            <span>
              <span className="t-title">
                {a.id} {a.simulated ? `(${t("simulated", lang)})` : ""} <code>{a.version || "—"}</code>
              </span>
              <small>{a.probe_error || (lang === "zh" ? "可用" : "available")}</small>
            </span>
            <span className="row">
              <span className={a.write_supported ? "tag" : "tag"} style={{ opacity: a.write_supported ? 1 : 0.45 }}>{t("write", lang)}</span>
              <span className="tag" style={{ opacity: a.read_only_supported ? 1 : 0.45 }}>{t("readOnly", lang)}</span>
              <span className="tag" style={{ opacity: a.resume_supported ? 1 : 0.45 }}>{t("resume", lang)}</span>
              <span className="tag" style={{ opacity: a.cancel_supported ? 1 : 0.45 }}>{t("cancel", lang)}</span>
            </span>
          </div>
        ))}
      </div>
      <div className="footer">
        <button onClick={onBack}>{t("backToDesk", lang)}</button>
      </div>
    </section>
  );
}

// ---------- history ----------
function History({ lang, tasks, onBack, onSelect }: { lang: Lang; tasks: Task[]; onBack: () => void; onSelect: (id: string) => void }) {
  return (
    <section className="panel">
      <div className="panel-head">
        <h2>{t("taskHistory", lang)}</h2>
        <p>{lang === "zh" ? "每次启动、退出与结果都绑定同一任务。" : "Launches, exits, and results belong to each task."}</p>
      </div>
      <div className="lineup tasklist">
        {tasks.map((task) => (
          <button key={task.task_id} className="taskrow" onClick={() => onSelect(task.task_id)}>
            <span>
              <span className="t-title">{task.title}</span>
              <small>
                <code>{task.task_id}</code> · {task.created_at?.slice(0, 16).replace("T", " ")}
              </small>
            </span>
            <span className="tag">{statusText(task.status, lang)}</span>
          </button>
        ))}
      </div>
      <div className="footer">
        <button onClick={onBack}>{t("backToDesk", lang)}</button>
      </div>
    </section>
  );
}

const root = createRoot(document.getElementById("root")!);
root.render(<App />);
