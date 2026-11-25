// Помощник: вопросы по базе знаний, черновик ответа и краткое содержание заявки, подсказка категории.
// Беседа живёт только в этой вкладке: сервер её не хранит.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AssistAnswer, type AssistCheck, type AssistSource, type AssistView } from "../../api/client";
import { useAssist } from "./useAssist";
import { Button, Card, ErrorBox, Field, Input, PageHeader, Spinner } from "../../ui";
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
          </Link>
        ) : s.ticket_number ? (
          <Link key={`t:${s.ticket_number}`} to="/tickets/$number" params={{ number: String(s.ticket_number) }} target="_blank" className="text-accent hover:underline">
            #{s.ticket_number} {s.title}
          </Link>
        ) : null,
      )}
    </div>
  );
}

type Turn = { id: number; role: "user" | "assistant"; content: string; sources?: AssistSource[] };

// Беседа — не длиннее 20 сообщений: старые уходят из запроса.
const MAX_TURNS = 20;
let nextTurn = 0;

/** Раздел «Помощник»: вопросы по базе знаний (инженеру — и по решённым заявкам). */
export function AssistantPage() {
  const { t } = useTranslation();
  const enabled = useAssist();
  const [turns, setTurns] = useState<Turn[]>([]);
  const [text, setText] = useState("");
  const send = useMutation({
    mutationFn: (history: Turn[]) =>
      ok<AssistAnswer>(api.POST("/api/helpdesk/assist/chat", { body: { messages: history.map(({ role, content }) => ({ role, content })) } })),
    onSuccess: (a) => setTurns((prev) => [...prev, { id: nextTurn++, role: "assistant" as const, content: a.text, sources: a.sources }].slice(-MAX_TURNS)),
  });
  const ask = () => {
    const q = text.trim();
    if (!q || send.isPending) return;
    const history: Turn[] = [...turns, { id: nextTurn++, role: "user" as const, content: q }].slice(-MAX_TURNS);
    setTurns(history);
    setText("");
    send.mutate(history);
  };
  const last = turns[turns.length - 1];
  return (
    <div className="mx-auto max-w-3xl space-y-4">
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
      {!enabled && <p className="rounded-lg bg-sunken px-3 py-2 text-sm text-muted">{t("assist.off")}</p>}
      {enabled && (
        <>
          <ul className="space-y-3">
            {turns.map((m) => (
              <li key={m.id} className={m.role === "user" ? "flex justify-end" : ""}>
                <div className={`max-w-[85%] rounded-xl px-4 py-2.5 text-sm ${m.role === "user" ? "bg-accent/10" : "border border-line bg-paper"}`}>
                  <span className="sr-only">{m.role === "user" ? t("assist.you") : t("assist.title")}: </span>
                  <div className="whitespace-pre-wrap break-words">{m.content}</div>
                  {m.sources && <Sources items={m.sources} />}
                </div>
              </li>
            ))}
            {send.isPending && <li className="text-sm text-muted">{t("assist.thinking")}</li>}
          </ul>
          <div className="sr-only" role="status" aria-live="polite">
            {send.isPending ? t("assist.thinking") : last?.role === "assistant" ? last.content : ""}
          </div>
          {turns.length === 0 && (
            <Card className="p-4 text-sm text-muted">
              <IconSparkles size={16} className="mb-2 text-accent" />
              {t("assist.empty")}
            </Card>
          )}
          {send.error && last?.role === "user" && <ErrorBox error={send.error} onRetry={() => send.mutate(turns)} />}
          <form
            className="space-y-2"
            onSubmit={(e) => {
              e.preventDefault();
              ask();
            }}
          >
            <textarea
              rows={3}
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
              className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm"
            />
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p id="assist-disclaimer" className="text-xs text-muted">
                {t("assist.disclaimer")}
              </p>
              <Button type="submit" variant="primary" disabled={!text.trim() || send.isPending}>
                {t("assist.send")}
              </Button>
            </div>
          </form>
        </>
      )}
    </div>
  );
}

/** Кнопка «Черновик ответа» у поля ответа инженера: текст вставляется в поле, ничего не отправляется. */
export function AssistDraftButton({ number, onInsert }: { number: number; onInsert: (text: string) => void }) {
  const { t } = useTranslation();
  const enabled = useAssist();
  const draft = useMutation({
    mutationFn: () => ok<AssistAnswer>(api.POST("/api/helpdesk/assist/tickets/{number}/draft", { params: { path: { number } } })),
    onSuccess: (a) => onInsert(a.text),
  });
  if (!enabled) return null;
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={() => draft.mutate()} disabled={draft.isPending}>
        <IconSparkles size={14} />
        {draft.isPending ? t("assist.thinking") : t("assist.draft")}
      </Button>
      {draft.error && <ErrorBox error={draft.error} />}
    </span>
  );
}

/** «Кратко о заявке» для инженера, который подхватывает чужую заявку. */
export function AssistSummary({ number }: { number: number }) {
  const { t } = useTranslation();
  const enabled = useAssist();
  const sum = useMutation({
    mutationFn: () => ok<AssistAnswer>(api.POST("/api/helpdesk/assist/tickets/{number}/summary", { params: { path: { number } } })),
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
      {sum.isPending && <p className="text-muted">{t("assist.thinking")}</p>}
      {sum.error && <ErrorBox error={sum.error} />}
      {sum.data && !sum.isPending && <div className="whitespace-pre-wrap break-words">{sum.data.text}</div>}
    </Card>
  );
}

/** «Подобрать категорию» в форме новой заявки. */
export function AssistCategoryButton({ title, description, onPick }: { title: string; description: string; onPick: (id: string) => void }) {
  const { t } = useTranslation();
  const enabled = useAssist();
  const [none, setNone] = useState(false);
  const asked = `${title.trim()}\n${description}`;
  const hint = useMutation({
    mutationFn: (text: { title: string; description: string }) => ok(api.POST("/api/helpdesk/assist/category", { body: text })),
    // Обработчик хука видит текущие поля формы: если, пока модель думала, текст поменяли, подсказка уже не про него.
    onSuccess: (r, sent) => {
      if (`${sent.title}\n${sent.description}` !== asked) return;
      setNone(!r.category_id);
      if (r.category_id) onPick(r.category_id);
    },
  });
  const fresh = hint.variables !== undefined && `${hint.variables.title}\n${hint.variables.description}` === asked;
  if (!enabled) return null;
  return (
    <span className="mt-1 inline-flex flex-wrap items-center gap-2 text-xs">
      <Button
        type="button"
        variant="ghost"
        className="min-h-7 px-2 text-xs"
        disabled={!title.trim() || hint.isPending}
        onClick={() => {
          setNone(false);
          hint.mutate({ title: title.trim(), description });
        }}
      >
        <IconSparkles size={14} />
        {hint.isPending ? t("assist.thinking") : t("assist.category")}
      </Button>
      <span role="status" className="text-muted">
        {none && fresh ? t("assist.noCategory") : ""}
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
