// Проблемы и изменения: списки, панель в карточке заявки, согласования и правила согласования.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Category, type ChangeApprovalItem, type ChangeRule, type ItilRecord, type Person, type Site, type TicketRow } from "../../api/client";
import { formatDate } from "../../format";
import { useMe } from "../../session";
import { Badge, Button, Card, ChipSelect, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { IconPlus } from "../../icons";
import { PriorityBadge, RichText, StatusBadge, TicketLink } from "./parts";
import { dyn, PRIORITIES } from "./helpdeskUtil";
import { CloseCodeSelect } from "./card";
import { PersonPicker } from "./NewTicketPage";

type Kind = "problem" | "change";
const RISKS = ["low", "medium", "high"] as const;
const area = "w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm";

/** ISO-время → значение поля `datetime-local` в часовом поясе браузера. */
function toLocal(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}
const fromLocal = (v: string) => (v ? new Date(v).toISOString() : null);

function approvalTone(a: string): "ok" | "bad" | "accent" | "muted" {
  return a === "approved" ? "ok" : a === "rejected" ? "bad" : a === "pending" ? "accent" : "muted";
}

/** Завести проблему или изменение; из инцидента — проблему, связанную с ним. */
export function NewItilDialog({ kind, open, onClose, incident, title: initial }: { kind: Kind; open: boolean; onClose: () => void; incident?: number; title?: string }) {
  const { t } = useTranslation();
  const nav = useNavigate();
  const qc = useQueryClient();
  const [title, setTitle] = useState(initial ?? "");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState("");
  const [site, setSite] = useState("");
  const [priority, setPriority] = useState("");
  const [risk, setRisk] = useState<string>("low");
  const cats = useQuery({ queryKey: ["categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories")), enabled: open });
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")), enabled: open });
  const create = useMutation({
    mutationFn: () =>
      ok<{ number: number }>(
        api.POST("/api/helpdesk/itil", {
          body: {
            kind,
            title,
            description,
            category_id: category || null,
            site_id: site || null,
            priority: priority || null,
            risk: kind === "change" ? risk : null,
            incident: incident ?? null,
          },
        }),
      ),
    onSuccess: (r) => {
      void qc.invalidateQueries({ queryKey: ["tickets"] });
      if (incident) void qc.invalidateQueries({ queryKey: ["ticket", incident] });
      onClose();
      void nav({ to: "/tickets/$number", params: { number: String(r.number) } });
    },
  });
  return (
    <Dialog open={open} onClose={onClose} title={dyn(t, `itil.new.${kind}`)} wide>
      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          create.mutate();
        }}
      >
        <Field label={t("itil.title")}>
          <Input autoFocus value={title} maxLength={300} onChange={(e) => setTitle(e.target.value)} />
        </Field>
        <Field label={t("itil.description")}>
          <textarea rows={4} className={area} value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("helpdesk.category")}>
            <Select value={category} onChange={(e) => setCategory(e.target.value)}>
              <option value="">—</option>
              {cats.data?.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </Select>
          </Field>
          <Field label={t("helpdesk.site")}>
            <Select value={site} onChange={(e) => setSite(e.target.value)}>
              <option value="">—</option>
              {sites.data?.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            </Select>
          </Field>
          <Field label={t("helpdesk.priorityLabel")}>
            <Select value={priority} onChange={(e) => setPriority(e.target.value)}>
              <option value="">{t("itil.byCategory")}</option>
              {PRIORITIES.map((p) => <option key={p} value={p}>{dyn(t, `helpdesk.priority.${p}`)}</option>)}
            </Select>
          </Field>
          {kind === "change" && (
            <Field label={t("itil.risk")}>
              <Select value={risk} onChange={(e) => setRisk(e.target.value)}>
                {RISKS.map((r) => <option key={r} value={r}>{dyn(t, `itil.risks.${r}`)}</option>)}
              </Select>
            </Field>
          )}
        </div>
        {create.error && <ErrorBox error={create.error} />}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" variant="primary" disabled={!title.trim() || create.isPending}>
            {t("itil.create")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function ItilList({ kind }: { kind: Kind }) {
  const { t, i18n } = useTranslation();
  const [status, setStatus] = useState("open");
  const [creating, setCreating] = useState(false);
  const limit = useMe().data?.prefs.page_size ?? 50;
  const list = useQuery({
    queryKey: ["tickets", "itil", kind, status, limit],
    queryFn: () => ok<TicketRow[]>(api.GET("/api/helpdesk/tickets", { params: { query: { scope: "all", kind, status, limit } } })),
    placeholderData: keepPreviousData,
    refetchInterval: 60_000,
  });
  return (
    <div>
      <PageHeader title={dyn(t, `itil.list.${kind}`)} subtitle={dyn(t, `itil.about.${kind}`)}>
        <ChipSelect label={t("helpdesk.statusLabel")} active={status !== "open"} value={status} onChange={(e) => setStatus(e.target.value)}>
          <option value="open">{t("helpdesk.filterOpen")}</option>
          <option value="all">{t("helpdesk.filterAll")}</option>
        </ChipSelect>
        <Button variant="primary" onClick={() => setCreating(true)}>
          <IconPlus size={14} />
          {dyn(t, `itil.new.${kind}`)}
        </Button>
      </PageHeader>
      {creating && <NewItilDialog kind={kind} open onClose={() => setCreating(false)} />}
      {list.error ? (
        <ErrorBox error={list.error} />
      ) : list.isPending ? (
        <Spinner variant="list" />
      ) : list.data.length === 0 ? (
        <Card>
          <Empty>{t("helpdesk.empty")}</Empty>
        </Card>
      ) : (
        <Card className="overflow-hidden">
          <ul className="divide-y divide-line">
            {list.data.map((r) => (
              <li key={r.id}>
                <TicketLink number={r.number} className="flex min-h-11 flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2 text-sm hover:bg-hover">
                  <span className="font-mono text-xs text-muted">#{r.number}</span>
                  <span className={`min-w-40 flex-1 truncate ${r.unread ? "font-semibold" : ""}`}>{r.title}</span>
                  <span className="hidden w-32 truncate text-muted lg:inline">{r.category_name}</span>
                  <PriorityBadge priority={r.priority} />
                  <StatusBadge status={r.status} />
                  <span className="hidden w-32 truncate text-muted md:inline">{r.assignee_name ?? "—"}</span>
                  <span className="text-xs text-muted">{formatDate(r.updated_at, i18n.language)}</span>
                </TicketLink>
              </li>
            ))}
          </ul>
        </Card>
      )}
    </div>
  );
}

export function ProblemsPage() {
  return <ItilList kind="problem" />;
}

export function ChangesPage() {
  return <ItilList kind="change" />;
}

function useRecord(number: number) {
  return useQuery({
    queryKey: ["itil", number],
    queryFn: () => ok<ItilRecord>(api.GET("/api/helpdesk/itil/{number}", { params: { path: { number } } })),
  });
}

function useInvalidate(number: number) {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: ["itil", number] });
    void qc.invalidateQueries({ queryKey: ["ticket", number] });
    void qc.invalidateQueries({ queryKey: ["tickets"] });
  };
}

/** Своё у проблемы или изменения — под описанием в карточке заявки. */
export function ItilPanel({ number, kind, closed }: { number: number; kind: Kind; closed: boolean }) {
  const q = useRecord(number);
  if (q.error) return <ErrorBox error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner />;
  return kind === "problem" && q.data.problem ? (
    <ProblemPanel number={number} r={q.data.problem} closed={closed} />
  ) : q.data.change ? (
    <ChangePanel number={number} r={q.data.change} closed={closed} />
  ) : null;
}

function ProblemPanel({ number, r, closed }: { number: number; r: NonNullable<ItilRecord["problem"]>; closed: boolean }) {
  const { t } = useTranslation();
  const done = useInvalidate(number);
  const [cause, setCause] = useState(r.cause);
  const [workaround, setWorkaround] = useState(r.workaround);
  const [known, setKnown] = useState(r.known_error);
  const [incident, setIncident] = useState("");
  const [resolving, setResolving] = useState(false);
  const [answer, setAnswer] = useState(r.workaround);
  const [code, setCode] = useState("");
  const save = useMutation({
    mutationFn: () => ok(api.PUT("/api/helpdesk/itil/{number}", { params: { path: { number } }, body: { cause, workaround, known_error: known, risk: "low", plan: "", rollback: "", planned_start: null, planned_end: null } })),
    meta: { toast: t("common.saved") },
    onSuccess: done,
  });
  const link = useMutation({
    mutationFn: () => ok(api.POST("/api/helpdesk/itil/{number}/incidents", { params: { path: { number } }, body: { number: Number(incident) } })),
    onSuccess: () => {
      setIncident("");
      done();
    },
  });
  const unlink = useMutation({
    mutationFn: (n: number) => ok(api.DELETE("/api/helpdesk/itil/{number}/incidents/{incident}", { params: { path: { number, incident: n } } })),
    onSuccess: done,
  });
  const resolveAll = useMutation({
    mutationFn: () => ok<{ resolved: number; failed: number[] }>(api.POST("/api/helpdesk/itil/{number}/resolve-incidents", { params: { path: { number } }, body: { body: answer, closure_code: code } })),
    onSuccess: () => {
      setResolving(false);
      done();
    },
  });
  const open = r.incidents.filter((i) => ["new", "assigned", "in_progress", "waiting", "waiting_approval", "on_hold"].includes(i.status));
  const dirty = cause !== r.cause || workaround !== r.workaround || known !== r.known_error;
  return (
    <Card className="space-y-3 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="font-medium">{t("itil.problemTitle")}</h2>
        {r.known_error && <Badge tone="accent">{t("itil.knownError")}</Badge>}
      </div>
      <Field label={t("itil.cause")}>
        <textarea rows={3} className={area} value={cause} disabled={closed} onChange={(e) => setCause(e.target.value)} />
      </Field>
      <Field label={t("itil.workaround")} hint={t("itil.workaroundHint")}>
        <textarea rows={3} className={area} value={workaround} disabled={closed} onChange={(e) => setWorkaround(e.target.value)} />
      </Field>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" className="h-4 w-4" checked={known} disabled={closed} onChange={(e) => setKnown(e.target.checked)} />
        {t("itil.markKnown")}
      </label>
      {save.error && <ErrorBox error={save.error} />}
      {!closed && (
        <div className="flex justify-end">
          <Button variant="primary" disabled={!dirty || save.isPending} onClick={() => save.mutate()}>
            {t("common.save")}
          </Button>
        </div>
      )}
      <div className="border-t border-line pt-3">
        <div className="mb-2 text-sm font-medium">{t("itil.incidents", { count: r.incidents.length })}</div>
        {r.incidents.length === 0 ? (
          <p className="text-sm text-muted">{t("itil.noIncidents")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {r.incidents.map((i) => (
              <li key={i.number} className="flex items-center gap-2">
                <TicketLink number={i.number} className="font-mono text-xs text-accent">
                  #{i.number}
                </TicketLink>
                <span className="min-w-0 flex-1 truncate">{i.title}</span>
                <StatusBadge status={i.status} />
                {!closed && (
                  <Button variant="ghost" className="min-h-7 px-2 text-xs" aria-label={t("itil.unlink", { n: i.number })} onClick={() => unlink.mutate(i.number)}>
                    {t("itil.unlinkShort")}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
        {!closed && (
          <form
            className="mt-2 flex flex-wrap items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              link.mutate();
            }}
          >
            <Input className="w-36" inputMode="numeric" aria-label={t("itil.incidentNumber")} placeholder={t("itil.incidentNumber")} value={incident} onChange={(e) => setIncident(e.target.value.replace(/\D/g, ""))} />
            <Button type="submit" disabled={!incident || link.isPending}>
              {t("itil.link")}
            </Button>
            {open.length > 0 && (
              <Button type="button" className="ml-auto" onClick={() => setResolving(true)}>
                {t("itil.resolveAll", { count: open.length })}
              </Button>
            )}
          </form>
        )}
        {(link.error || unlink.error) && <ErrorBox error={link.error ?? unlink.error} />}
        {resolveAll.data && resolveAll.data.failed.length > 0 && <p className="mt-2 text-sm text-bad">{t("itil.resolveFailed", { list: resolveAll.data.failed.map((n) => `#${n}`).join(", ") })}</p>}
      </div>
      <Dialog open={resolving} onClose={() => setResolving(false)} title={t("itil.resolveAll", { count: open.length })}>
        <div className="space-y-3">
          <p className="text-sm text-muted">{t("itil.resolveAllHint")}</p>
          <Field label={t("helpdesk.resolveComment")}>
            <textarea rows={4} className={area} value={answer} onChange={(e) => setAnswer(e.target.value)} />
          </Field>
          <CloseCodeSelect value={code} onChange={setCode} />
          {resolveAll.error && <ErrorBox error={resolveAll.error} />}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setResolving(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="primary" disabled={!answer.trim() || resolveAll.isPending} onClick={() => resolveAll.mutate()}>
              {t("itil.resolveAllRun")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

function ChangePanel({ number, r, closed }: { number: number; r: NonNullable<ItilRecord["change"]>; closed: boolean }) {
  const { t, i18n } = useTranslation();
  const done = useInvalidate(number);
  const [risk, setRisk] = useState(r.risk);
  const [plan, setPlan] = useState(r.plan);
  const [rollback, setRollback] = useState(r.rollback);
  const [start, setStart] = useState(toLocal(r.planned_start));
  const [end, setEnd] = useState(toLocal(r.planned_end));
  const body = { risk, plan, rollback, planned_start: fromLocal(start), planned_end: fromLocal(end), cause: "", workaround: "", known_error: false };
  const dirty = risk !== r.risk || plan !== r.plan || rollback !== r.rollback || start !== toLocal(r.planned_start) || end !== toLocal(r.planned_end);
  const save = useMutation({
    mutationFn: () => ok(api.PUT("/api/helpdesk/itil/{number}", { params: { path: { number } }, body })),
    meta: { toast: t("common.saved") },
    onSuccess: done,
  });
  const submit = useMutation({
    mutationFn: () => ok<{ approval: string; approvers: number }>(api.POST("/api/helpdesk/itil/{number}/submit", { params: { path: { number } } })),
    meta: { toast: t("itil.submitted") },
    onSuccess: done,
  });
  const rounds = [...new Set(r.approvals.map((a) => a.round))];
  return (
    <Card className="space-y-3 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="font-medium">{t("itil.changeTitle")}</h2>
        <Badge tone={approvalTone(r.approval)}>{dyn(t, `itil.approval.${r.approval}`)}</Badge>
      </div>
      {(r.approval === "approved" || r.approval === "pending") && !closed && <p className="text-sm text-muted">{t("itil.editResets")}</p>}
      <div className="grid gap-3 sm:grid-cols-3">
        <Field label={t("itil.risk")}>
          <Select value={risk} disabled={closed} onChange={(e) => setRisk(e.target.value)}>
            {RISKS.map((x) => <option key={x} value={x}>{dyn(t, `itil.risks.${x}`)}</option>)}
          </Select>
        </Field>
        <Field label={t("itil.start")}>
          <Input type="datetime-local" value={start} disabled={closed} onChange={(e) => setStart(e.target.value)} />
        </Field>
        <Field label={t("itil.end")}>
          <Input type="datetime-local" value={end} disabled={closed} onChange={(e) => setEnd(e.target.value)} />
        </Field>
      </div>
      <Field label={t("itil.plan")} hint={t("itil.planHint")}>
        <textarea rows={4} className={area} value={plan} disabled={closed} onChange={(e) => setPlan(e.target.value)} />
      </Field>
      <Field label={t("itil.rollback")} hint={t("itil.rollbackHint")}>
        <textarea rows={3} className={area} value={rollback} disabled={closed} onChange={(e) => setRollback(e.target.value)} />
      </Field>
      {(save.error || submit.error) && <ErrorBox error={save.error ?? submit.error} />}
      {!closed && (
        <div className="flex flex-wrap justify-end gap-2">
          <Button disabled={!dirty || save.isPending} onClick={() => save.mutate()}>
            {t("common.save")}
          </Button>
          {(r.approval === "draft" || r.approval === "rejected") && (
            <Button variant="primary" disabled={dirty || submit.isPending} title={dirty ? t("itil.saveFirst") : undefined} onClick={() => submit.mutate()}>
              {t("itil.submit")}
            </Button>
          )}
        </div>
      )}
      {r.approvals.length > 0 && (
        <div className="border-t border-line pt-3">
          <div className="mb-2 text-sm font-medium">{t("itil.approvals")}</div>
          {rounds.map((round) => (
            <div key={round} className="mb-2">
              <div className="text-xs text-muted">{t("itil.round", { n: round })}</div>
              <ul className="space-y-1 text-sm">
                {r.approvals
                  .filter((a) => a.round === round)
                  .map((a) => (
                    <li key={a.id} className="flex flex-wrap items-center gap-2">
                      <span className="min-w-0 flex-1 truncate">{a.approver_name ?? "—"}</span>
                      <Badge tone={approvalTone(a.status)}>{dyn(t, `itil.decision.${a.status}`)}</Badge>
                      {a.decided_at && <span className="text-xs text-muted">{formatDate(a.decided_at, i18n.language)}</span>}
                      {a.comment && <span className="w-full text-xs text-muted">«{a.comment}»</span>}
                    </li>
                  ))}
              </ul>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}

/** Согласования изменений на странице «Согласования». */
export function ChangeApprovals() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["change-approvals"], queryFn: () => ok<ChangeApprovalItem[]>(api.GET("/api/helpdesk/change-approvals")) });
  const pending = q.data?.filter((a) => a.status === "pending") ?? [];
  const decided = q.data?.filter((a) => a.status !== "pending") ?? [];
  if (q.error) return <ErrorBox error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data || q.data.length === 0) return null;
  return (
    <section className="space-y-3">
      <h2 className="text-sm font-semibold text-muted">{t("itil.changeApprovals")}</h2>
      {pending.map((a) => (
        <ChangeApprovalCard key={a.id} a={a} />
      ))}
      {decided.length > 0 && (
        <Card>
          <ul className="divide-y divide-line text-sm">
            {decided.map((a) => (
              <li key={a.id} className="flex flex-wrap items-center gap-2 px-3 py-2">
                <span className="font-mono text-xs text-muted">#{a.number}</span>
                <span className="min-w-0 flex-1 truncate">{a.title}</span>
                <Badge tone={approvalTone(a.status)}>{dyn(t, `itil.decision.${a.status}`)}</Badge>
              </li>
            ))}
          </ul>
        </Card>
      )}
    </section>
  );
}

function ChangeApprovalCard({ a }: { a: ChangeApprovalItem }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [comment, setComment] = useState("");
  const decide = useMutation({
    mutationFn: (approve: boolean) => ok(api.POST("/api/helpdesk/change-approvals/{id}", { params: { path: { id: a.id } }, body: { approve, comment: comment.trim() || null } })),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: ["change-approvals"] });
      void qc.invalidateQueries({ queryKey: ["notifications"] });
    },
  });
  const when = [a.planned_start, a.planned_end].filter(Boolean).map((d) => formatDate(d as string, i18n.language)).join(" — ");
  return (
    <Card className="space-y-2 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-xs text-muted">#{a.number}</span>
        <span className="font-medium">{a.title}</span>
        <Badge tone={a.risk === "high" ? "bad" : a.risk === "medium" ? "accent" : "muted"}>{dyn(t, `itil.risks.${a.risk}`)}</Badge>
      </div>
      <div className="text-xs text-muted">
        {[a.category_name, a.assignee_name && t("itil.leads", { name: a.assignee_name }), when].filter(Boolean).join(" · ")}
      </div>
      {a.description && <RichText text={a.description} />}
      <div>
        <div className="text-xs font-medium text-muted">{t("itil.plan")}</div>
        <RichText text={a.plan} />
      </div>
      {a.rollback && (
        <div>
          <div className="text-xs font-medium text-muted">{t("itil.rollback")}</div>
          <RichText text={a.rollback} />
        </div>
      )}
      <Input aria-label={t("itil.decisionComment")} placeholder={t("itil.decisionComment")} value={comment} onChange={(e) => setComment(e.target.value)} />
      {decide.error && <ErrorBox error={decide.error} />}
      <div className="flex flex-wrap justify-end gap-2">
        <Button disabled={decide.isPending || !comment.trim()} title={!comment.trim() ? t("itil.rejectNeedsComment") : undefined} onClick={() => decide.mutate(false)}>
          {t("itil.reject")}
        </Button>
        <Button variant="primary" disabled={decide.isPending} onClick={() => decide.mutate(true)}>
          {t("itil.approve")}
        </Button>
      </div>
    </Card>
  );
}

/** Настройки: кто согласует изменения. */
export function ChangeRulesPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const rules = useQuery({ queryKey: ["change-rules"], queryFn: () => ok<ChangeRule[]>(api.GET("/api/helpdesk/change-rules")) });
  const cats = useQuery({ queryKey: ["categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories")) });
  const [category, setCategory] = useState("");
  const [risk, setRisk] = useState<string>("medium");
  const [person, setPerson] = useState<Person | null>(null);
  const refresh = () => void qc.invalidateQueries({ queryKey: ["change-rules"] });
  const add = useMutation({
    mutationFn: () => ok(api.POST("/api/helpdesk/change-rules", { body: { category_id: category || null, min_risk: risk, person_id: person?.id ?? "" } })),
    meta: { toast: t("common.saved") },
    onSuccess: () => {
      setPerson(null);
      refresh();
    },
  });
  const remove = useMutation({
    mutationFn: (id: string) => ok(api.DELETE("/api/helpdesk/change-rules/{id}", { params: { path: { id } } })),
    onSuccess: refresh,
  });
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <PageHeader title={t("admin.menu.changeRules")} subtitle={t("itil.rulesHint")} />
      <Card className="space-y-3 p-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("helpdesk.category")}>
            <Select value={category} onChange={(e) => setCategory(e.target.value)}>
              <option value="">{t("itil.anyCategory")}</option>
              {cats.data?.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </Select>
          </Field>
          <Field label={t("itil.fromRisk")}>
            <Select value={risk} onChange={(e) => setRisk(e.target.value)}>
              {RISKS.map((x) => <option key={x} value={x}>{dyn(t, `itil.risks.${x}`)}</option>)}
            </Select>
          </Field>
        </div>
        <Field label={t("itil.approver")}>
          <PersonPicker value={person} onPick={setPerson} />
        </Field>
        {add.error && <ErrorBox error={add.error} />}
        <div className="flex justify-end">
          <Button variant="primary" disabled={!person || add.isPending} onClick={() => add.mutate()}>
            <IconPlus size={14} />
            {t("itil.addRule")}
          </Button>
        </div>
      </Card>
      {rules.error && <ErrorBox error={rules.error} />}
      {rules.isPending && <Spinner />}
      {rules.data && rules.data.length === 0 && (
        <Card>
          <Empty hint={t("itil.noRulesHint")}>{t("itil.noRules")}</Empty>
        </Card>
      )}
      {rules.data && rules.data.length > 0 && (
        <Card>
          <ul className="divide-y divide-line text-sm">
            {rules.data.map((r) => (
              <li key={r.id} className="flex flex-wrap items-center gap-2 px-4 py-2">
                <span className="w-48 truncate">{r.category_name ?? t("itil.anyCategory")}</span>
                <span className="w-40 text-muted">{t("itil.fromRiskValue", { risk: dyn(t, `itil.risks.${r.min_risk}`) })}</span>
                <span className="min-w-0 flex-1 truncate font-medium">{r.approver_name ?? "—"}</span>
                <Button variant="ghost" className="min-h-7 px-2 text-xs" onClick={() => remove.mutate(r.id)}>
                  {t("common.delete")}
                </Button>
              </li>
            ))}
          </ul>
        </Card>
      )}
      {remove.error && <ErrorBox error={remove.error} />}
    </div>
  );
}
