// Помощник: вопросы по базе знаний, черновик ответа и краткое содержание заявки, подсказка категории.
// Беседа живёт только в этой вкладке: сервер её не хранит.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AssistAnswer, type AssistCheck, type AssistSource, type AssistView } from "../../api/client";
import { useAssist, useAssistState } from "./useAssist";
import { useMe } from "../../session";
import { RichText } from "./parts";
import { Button, Card, Empty, ErrorBox, Field, Input, PageHeader, Spinner } from "../../ui";
import { IconSparkles } from "../../icons";

function Sources({ items }: { items: AssistSource[] }) {
  const { t } = useTranslation();
  if (items.length === 0) return null;
  return (
    <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted">
      <span>{t("assist.sources")}:</span>
      {items.map((s) =>
        s.kind === "kb" && s.article_id ? (
          <Link key={`kb:${s.article_id}`} to="/kb/$id" params={{ id: s.article_id }} target="_blank" className="text-accent hover:underline">
            {s.title}
            <span className="sr-only"> {t("extensions.newTab")}</span>
          </Link>
        ) : s.ticket_number ? (
          <Link key={`t:${s.ticket_number}`} to="/tickets/$number" params={{ number: String(s.ticket_number) }} target="_blank" className="text-accent hover:underline">
            #{s.ticket_number} {s.title}
            <span className="sr-only"> {t("extensions.newTab")}</span>
          </Link>
        ) : null,
      )}
    </div>
  );
}

type Turn = { id: number; role: "user" | "assistant"; content: string; sources?: AssistSource[] };

// В запрос уходят последние 20 сообщений; на экране остаётся вся беседа.
const MAX_TURNS = 20;
let nextTurn = 0;
/** Разговор с помощником между заходами в раздел (до перезагрузки вкладки; выход из портала её перезагружает). */
let saved: { user: string; turns: Turn[]; text: string } = { user: "", turns: [], text: "" };

const area = "w-full resize-none rounded-lg border border-control bg-paper px-3 py-2 text-sm focus:border-accent";

/** Сколько секунд идёт запрос (от `submittedAt` мутации): без видеокарты ответ занимает до полутора минут. */
function useElapsed(running: boolean, since: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [running]);
  return running ? Math.max(0, Math.floor((now - since) / 1000)) : 0;
}

const mmss = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

/** Раздел «ИИ-помощник»: вопросы по базе знаний (инженеру — и по решённым заявкам). */
export function AssistantPage() {
  const { t } = useTranslation();
  const state = useAssistState();
  const user = useMe().data?.user_id ?? "";
  const kept = saved.user === user ? saved : null;
  const [turns, setTurns] = useState<Turn[]>(() => kept?.turns ?? []);
  const [text, setText] = useState(() => kept?.text ?? "");
  const abort = useRef<AbortController | null>(null);
  // Разговор переживает переход в другой раздел. Неотвеченный вопрос при уходе прерывается и
  // возвращается в поле ввода: модель не держит очередь ради страницы, которую закрыли.
  useEffect(() => {
    const last = turns[turns.length - 1];
    saved = last?.role === "user" && !text ? { user, turns: turns.slice(0, -1), text: last.content } : { user, turns, text };
  }, [user, turns, text]);
  useEffect(() => () => abort.current?.abort(), []);
  const box = useRef<HTMLTextAreaElement>(null);
  const send = useMutation({
    mutationFn: (history: Turn[]) => {
      abort.current = new AbortController();
      const messages = history.slice(-MAX_TURNS).map(({ role, content }) => ({ role, content }));
      return ok<AssistAnswer>(api.POST("/api/helpdesk/assist/chat", { body: { messages }, signal: abort.current.signal }));
    },
    onSuccess: (a) => setTurns((prev) => [...prev, { id: nextTurn++, role: "assistant" as const, content: a.text, sources: a.sources }]),
  });
  const elapsed = useElapsed(send.isPending, send.submittedAt);
  const ask = () => {
    const q = text.trim();
    if (!q || send.isPending) return;
    const history: Turn[] = [...turns, { id: nextTurn++, role: "user" as const, content: q }];
    setTurns(history);
    setText("");
    send.mutate(history);
  };
  // «Остановить»: вопрос возвращается в поле ввода, его можно поправить и задать снова.
  const stop = () => {
    abort.current?.abort();
    const last = turns[turns.length - 1];
    if (last?.role === "user") {
      setTurns(turns.slice(0, -1));
      setText(last.content);
    }
    send.reset();
    box.current?.focus();
  };
  const end = useRef<HTMLLIElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end", behavior: "smooth" });
  }, [turns.length, send.isPending, send.error]);
  const last = turns[turns.length - 1];
  if (state.pending) return <Spinner />;
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-3">
      <PageHeader title={t("assist.title")} subtitle={t("assist.subtitle")}>
        {turns.length > 0 && (
          <Button
            onClick={() => {
              setTurns([]);
              send.reset();
            }}
            disabled={send.isPending}
          >
            {t("assist.newChat")}
          </Button>
        )}
      </PageHeader>
      {!state.enabled && <p className="rounded-lg bg-sunken px-3 py-2 text-sm text-muted">{t("assist.off")}</p>}
      {state.enabled && (
        <Card className="flex h-[calc(100dvh-12rem)] min-h-96 flex-col">
          <ul className="pult-scroll min-h-0 flex-1 space-y-2 overflow-y-auto px-3 py-3" aria-label={t("assist.title")}>
            {turns.length === 0 && (
              <li>
                <Empty icon={<IconSparkles size={22} />} hint={t("assist.empty")}>
                  {t("assist.title")}
                </Empty>
              </li>
            )}
            {turns.map((m) => (
              <li key={m.id} className={`flex ${m.role === "user" ? "justify-end" : "justify-start"}`}>
                <div className={`max-w-[85%] rounded-2xl px-3 py-2 text-sm ${m.role === "user" ? "bg-accent/15 sm:max-w-[70%]" : "bg-hover"}`}>
                  <span className="sr-only">{m.role === "user" ? t("assist.you") : t("assist.title")}: </span>
                  {m.role === "user" ? <p className="break-words whitespace-pre-wrap">{m.content}</p> : <RichText text={m.content} keepBreaks={false} />}
                  {m.sources && <Sources items={m.sources} />}
                </div>
              </li>
            ))}
            {send.isPending && (
              <li className="flex justify-start">
                <div className="max-w-[85%] rounded-2xl bg-hover px-3 py-2 text-sm">
                  <div className="flex items-center gap-2">
                    <span className="animate-pulse text-accent" aria-hidden="true">
                      <IconSparkles size={16} />
                    </span>
                    <span className="tabular-nums">{t("assist.thinkingFor", { time: mmss(elapsed) })}</span>
                    <Button variant="ghost" className="ml-1 min-h-7 px-2 text-xs" onClick={stop}>
                      {t("assist.stop")}
                    </Button>
                  </div>
                  {elapsed >= 10 && <p className="mt-1 text-xs text-muted">{t("assist.wait")}</p>}
                </div>
              </li>
            )}
            {send.error && last?.role === "user" && (
              <li>
                <ErrorBox error={send.error} onRetry={() => send.mutate(turns)} />
              </li>
            )}
            <li ref={end} aria-hidden="true" />
          </ul>
          <div className="sr-only" role="status" aria-live="polite">
            {send.isPending ? t("assist.thinking") : last?.role === "assistant" ? last.content : ""}
          </div>
          <form
            className="border-t border-line p-2"
            onSubmit={(e) => {
              e.preventDefault();
              ask();
            }}
          >
            <div className="flex items-end gap-2">
              <textarea
                ref={box}
                rows={2}
                value={text}
                maxLength={4000}
                aria-label={t("assist.question")}
                aria-describedby="assist-disclaimer"
                placeholder={t("assist.question")}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    ask();
                  }
                }}
                className={area}
              />
              <Button type="submit" variant="primary" disabled={!text.trim() || send.isPending}>
                {t("assist.send")}
              </Button>
            </div>
            <p id="assist-disclaimer" className="mt-1 px-1 text-xs text-muted">
              {t("assist.disclaimer")}
            </p>
          </form>
        </Card>
      )}
    </div>
  );
}

/** Ожидание ответа модели: таймер и «Остановить» (на сервере без видеокарты ответ идёт больше минуты). */
function Waiting({ since, onStop }: { since: number; onStop: () => void }) {
  const { t } = useTranslation();
  const secs = useElapsed(true, since);
  return (
    <span className="inline-flex items-center gap-1 text-xs text-muted">
      <span className="animate-pulse text-accent" aria-hidden="true">
        <IconSparkles size={14} />
      </span>
      <span className="tabular-nums">{t("assist.thinkingFor", { time: mmss(secs) })}</span>
      <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={onStop}>
        {t("assist.stop")}
      </Button>
    </span>
  );
}

/** Запрос к модели, который можно остановить: прерывается и при уходе со страницы. */
function useAbort() {
  const ctrl = useRef<AbortController | null>(null);
  useEffect(() => () => ctrl.current?.abort(), []);
  return {
    signal: () => {
      ctrl.current?.abort();
      ctrl.current = new AbortController();
      return ctrl.current.signal;
    },
    stop: () => ctrl.current?.abort(),
  };
}

/** Кнопка «Черновик ответа» у поля ответа инженера: текст вставляется в поле, ничего не отправляется. */
export function AssistDraftButton({ number, onInsert }: { number: number; onInsert: (text: string) => void }) {
  const { t } = useTranslation();
  const enabled = useAssist();
  const abort = useAbort();
  const draft = useMutation({
    mutationFn: () => ok<AssistAnswer>(api.POST("/api/helpdesk/assist/tickets/{number}/draft", { params: { path: { number } }, signal: abort.signal() })),
    onSuccess: (a) => onInsert(a.text),
  });
  if (!enabled) return null;
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      {draft.isPending ? (
        <Waiting
          since={draft.submittedAt}
          onStop={() => {
            abort.stop();
            draft.reset();
          }}
        />
      ) : (
        <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={() => draft.mutate()}>
          <IconSparkles size={14} />
          {t("assist.draft")}
        </Button>
      )}
      {draft.error && <ErrorBox error={draft.error} />}
    </span>
  );
}

/** «Кратко о заявке» для инженера, который подхватывает чужую заявку. */
export function AssistSummary({ number }: { number: number }) {
  const { t } = useTranslation();
  const enabled = useAssist();
  const abort = useAbort();
  const sum = useMutation({
    mutationFn: () => ok<AssistAnswer>(api.POST("/api/helpdesk/assist/tickets/{number}/summary", { params: { path: { number } }, signal: abort.signal() })),
  });
  if (!enabled) return null;
  if (!sum.data && !sum.isPending && !sum.error) {
    return (
      <div>
        <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={() => sum.mutate()}>
          <IconSparkles size={14} />
          {t("assist.summary")}
        </Button>
      </div>
    );
  }
  return (
    <Card className="p-4 text-sm">
      <div className="mb-1 flex items-center gap-2 font-medium">
        <IconSparkles size={14} className="text-accent" />
        {t("assist.summary")}
        <Button variant="ghost" className="ml-auto min-h-7 px-2 text-xs" onClick={() => sum.mutate()} disabled={sum.isPending}>
          {t("assist.again")}
        </Button>
      </div>
      {sum.isPending && (
        <Waiting
          since={sum.submittedAt}
          onStop={() => {
            abort.stop();
            sum.reset();
          }}
        />
      )}
      {sum.error && <ErrorBox error={sum.error} />}
      {sum.data && !sum.isPending && <RichText text={sum.data.text} keepBreaks={false} />}
    </Card>
  );
}

/** «Подобрать категорию» в форме новой заявки. Подсказка не перебивает выбор, сделанный вручную, пока модель думала. */
export function AssistCategoryButton({ title, description, current, onPick }: { title: string; description: string; current: string; onPick: (id: string) => void }) {
  const { t } = useTranslation();
  const enabled = useAssist();
  const abort = useAbort();
  const [none, setNone] = useState(false);
  const asked = `${title.trim()}\n${description}`;
  // Категория на момент нажатия: если её сменили вручную, ответ модели уже не нужен.
  const pickedBefore = useRef(current);
  const hint = useMutation({
    mutationFn: (text: { title: string; description: string }) => ok(api.POST("/api/helpdesk/assist/category", { body: text, signal: abort.signal() })),
    // Обработчик хука видит текущие поля формы: если, пока модель думала, текст поменяли, подсказка уже не про него.
    onSuccess: (r, sent) => {
      if (`${sent.title}\n${sent.description}` !== asked || current !== pickedBefore.current) return;
      setNone(!r.category_id);
      if (r.category_id) onPick(r.category_id);
    },
  });
  const fresh = hint.variables !== undefined && `${hint.variables.title}\n${hint.variables.description}` === asked;
  if (!enabled) return null;
  return (
    <span className="mt-1 inline-flex flex-wrap items-center gap-2 text-xs">
      {hint.isPending ? (
        <Waiting
          since={hint.submittedAt}
          onStop={() => {
            abort.stop();
            hint.reset();
          }}
        />
      ) : (
        <Button
          type="button"
          variant="ghost"
          className="min-h-7 px-2 text-xs"
          disabled={!title.trim()}
          title={title.trim() ? undefined : t("assist.categoryNeedsTitle")}
          onClick={() => {
            setNone(false);
            pickedBefore.current = current;
            hint.mutate({ title: title.trim(), description });
          }}
        >
          <IconSparkles size={14} />
          {t("assist.category")}
        </Button>
      )}
      <span role="status" className="text-muted">
        {!title.trim() ? t("assist.categoryNeedsTitle") : none && fresh ? t("assist.noCategory") : ""}
      </span>
      {hint.error && (
        <span role="alert" className="text-bad">
          {t("assist.unavailable")}
        </span>
      )}
    </span>
  );
}

const num = (s: string, fallback: number) => (s.trim() !== "" && Number.isFinite(Number(s)) ? Math.trunc(Number(s)) : fallback);

/** Настройки помощника (только суперадминистратор): сервер модели, модель, ключ API. */
export function AssistCard() {
  const q = useQuery({ queryKey: ["assist-admin"], queryFn: () => ok<AssistView>(api.GET("/api/admin/assist")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return <AssistForm key={q.dataUpdatedAt} data={q.data} />;
}

function AssistForm({ data }: { data: AssistView }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data.settings);
  const [timeout, setTimeoutText] = useState(String(data.settings.timeout_secs));
  const [tokens, setTokens] = useState(String(data.settings.max_tokens));
  const [key, setKey] = useState("");
  const [dropKey, setDropKey] = useState(false);
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () =>
      ok(
        api.PUT("/api/admin/assist", {
          body: { settings: { ...v, timeout_secs: num(timeout, v.timeout_secs), max_tokens: num(tokens, v.max_tokens) }, api_key: dropKey ? "" : key || null },
        }),
      ),
    onSuccess: () => {
      setKey("");
      setDropKey(false);
      void qc.invalidateQueries({ queryKey: ["assist-admin"] });
      void qc.invalidateQueries({ queryKey: ["assist"] });
    },
  });
  const test = useMutation({ mutationFn: () => ok<AssistCheck>(api.POST("/api/admin/assist/test")) });
  return (
    <div className="mx-auto max-w-2xl">
      <Card className="grid gap-4 p-5 sm:grid-cols-2">
        <div className="sm:col-span-2">
          <h2 className="font-semibold">{t("admin.assist.title")}</h2>
          <p className="mt-1 text-xs text-muted">{t("admin.assist.hint")}</p>
        </div>
        <label className="inline-flex items-center gap-2 text-sm font-medium sm:col-span-2">
          <input type="checkbox" checked={v.enabled} onChange={(e) => setV({ ...v, enabled: e.target.checked })} />
          {t("admin.assist.enabled")}
        </label>
        <div className="sm:col-span-2">
          <Field label={t("admin.assist.url")} hint={t("admin.assist.urlHint")}>
            <Input value={v.base_url} onChange={(e) => setV({ ...v, base_url: e.target.value })} placeholder="http://llm.example.local:8000/v1" />
          </Field>
        </div>
        <Field label={t("admin.assist.model")}>
          <Input value={v.model} maxLength={200} onChange={(e) => setV({ ...v, model: e.target.value })} placeholder="qwen2.5-14b-instruct" />
        </Field>
        <Field label={t("admin.assist.key")} hint={data.has_key ? t("admin.ad.passwordKept") : t("admin.assist.noKey")}>
          <Input type="password" autoComplete="new-password" value={key} disabled={dropKey} onChange={(e) => setKey(e.target.value)} />
          {data.has_key && (
            <label className="mt-1 inline-flex items-center gap-2 text-xs text-muted">
              <input type="checkbox" checked={dropKey} onChange={(e) => setDropKey(e.target.checked)} />
              {t("admin.assist.dropKey")}
            </label>
          )}
        </Field>
        <Field label={t("admin.assist.timeout")}>
          <Input type="number" min={5} max={300} value={timeout} onChange={(e) => setTimeoutText(e.target.value)} />
        </Field>
        <Field label={t("admin.assist.maxTokens")}>
          <Input type="number" min={64} max={8192} value={tokens} onChange={(e) => setTokens(e.target.value)} />
        </Field>
        <p className="text-xs text-muted sm:col-span-2">{t("admin.assist.privacy")}</p>
        <div className="flex flex-wrap items-center gap-2 sm:col-span-2">
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
            {t("common.save")}
          </Button>
          <Button onClick={() => test.mutate()} disabled={test.isPending || !data.settings.base_url}>
            {t("admin.assist.test")}
          </Button>
          {test.data && (
            <span role="status" className={`text-sm ${test.data.ok ? "text-ok" : "text-bad"}`}>
              {test.data.ok ? t("admin.assist.testOk", { ms: test.data.ms }) : (test.data.error ?? t("assist.unavailable"))}
            </span>
          )}
        </div>
        {(save.error || test.error) && (
          <div className="sm:col-span-2">
            <ErrorBox error={save.error ?? test.error} />
          </div>
        )}
      </Card>
    </div>
  );
}
