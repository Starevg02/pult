// Админка сервиса: календари и SLA, группы и правила, шаблоны, типы оборудования, интеграции.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  api,
  ok,
  type Alert,
  type AlertSource,
  type ApiToken,
  type AssetType,
  type Calendar,
  type Category,
  type CreatedSource,
  type CreatedToken,
  type EngineerView,
  type Group,
  type Lifecycle,
  type Person,
  type Recurring,
  type RelationType,
  type ResponseTemplate,
  type Rule,
  type Site,
  type SlaPolicy,
  type SpecialDay,
  type TicketTemplate,
  type Webhook,
} from "../../api/client";
import { formatDate } from "../../format";
import { useMe } from "../../session";
import { Badge, Button, Card, CopyButton, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { fullName } from "../phonebookUtil";
import { useGroups } from "../helpdesk/cardUtil";
import { dyn, PRIORITIES } from "../helpdesk/helpdeskUtil";
import { PersonPicker } from "../helpdesk/NewTicketPage";
import { TicketLink } from "../helpdesk/parts";
import { LifecycleEditor } from "../assets/CmdbParts";

const DAYS = [1, 2, 3, 4, 5, 6, 7];
const area = "w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm";

function useList<T>(key: string, fn: () => Promise<T[]>) {
  return useQuery({ queryKey: [key], queryFn: fn });
}

function useSave(keys: string[], onDone: () => void) {
  const qc = useQueryClient();
  return <T,>(fn: (v: T) => Promise<unknown>) =>
    // eslint-disable-next-line react-hooks/rules-of-hooks -- фабрика вызывается на верхнем уровне компонента
    useMutation({
      mutationFn: fn,
      onSuccess: () => {
        for (const k of keys) void qc.invalidateQueries({ queryKey: [k] });
        onDone();
      },
    });
}

function Footer({ onClose, onSave, disabled, onDelete, error }: { onClose: () => void; onSave: () => void; disabled: boolean; onDelete?: () => void; error: unknown }) {
  const { t } = useTranslation();
  return (
    <>
      {!!error && <ErrorBox error={error} />}
      <div className="mt-3 flex gap-2">
        {onDelete && (
          <Button variant="danger" onClick={() => window.confirm(t("svc.deleteConfirm")) && onDelete()}>
            {t("common.delete")}
          </Button>
        )}
        <Button className="ml-auto" onClick={onClose}>
          {t("common.cancel")}
        </Button>
        <Button variant="primary" disabled={disabled} onClick={onSave}>
          {t("common.save")}
        </Button>
      </div>
    </>
  );
}

function ListSection<T extends { id: string }>({ title, items, onNew, row, empty }: { title: string; items: T[] | undefined; onNew?: () => void; row: (x: T) => ReactNode; empty: string }) {
  const { t } = useTranslation();
  return (
    <section>
      <PageHeader title={title}>
        {onNew && (
          <Button variant="primary" onClick={onNew}>
            {t("common.add")}
          </Button>
        )}
      </PageHeader>
      {!items ? (
        <Spinner />
      ) : items.length === 0 ? (
        <Card>
          <Empty>{empty}</Empty>
        </Card>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {items.map((x) => (
              <li key={x.id}>{row(x)}</li>
            ))}
          </ul>
        </Card>
      )}
    </section>
  );
}

const rowBtn = "flex w-full flex-wrap items-center gap-2 px-4 py-2 text-left text-sm hover:bg-hover";

// ---------- Календари и SLA ----------

type Hours = { day: number; start: string; end: string };

function CalendarDialog({ value, onClose }: { value: Calendar | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const c = value === "new" ? null : value;
  const [name, setName] = useState(c?.name ?? "");
  const [hours, setHours] = useState<Hours[]>((c?.hours as Hours[] | undefined) ?? [1, 2, 3, 4, 5].map((day) => ({ day, start: "09:00", end: "18:00" })));
  const [days, setDays] = useState<SpecialDay[] | null>(null);
  const [importText, setImportText] = useState("");
  const special = useQuery({
    queryKey: ["calendar-days", c?.id],
    queryFn: async () => {
      const r = await ok<SpecialDay[]>(api.GET("/api/admin/helpdesk/calendars/{id}/days", { params: { path: { id: c?.id ?? "" } } }));
      setDays(r);
      return r;
    },
    enabled: !!c,
  });
  const action = useSave(["calendars"], onClose);
  const save = action(async () => {
    const body = { name, hours };
    let id = c?.id;
    if (id) await ok(api.PUT("/api/admin/helpdesk/calendars/{id}", { params: { path: { id } }, body }));
    else id = ((await ok(api.POST("/api/admin/helpdesk/calendars", { body }))) as { id: string }).id;
    if (days) await ok(api.PUT("/api/admin/helpdesk/calendars/{id}/days", { params: { path: { id } }, body: days }));
  });
  const del = action(() => ok(api.DELETE("/api/admin/helpdesk/calendars/{id}", { params: { path: { id: c?.id ?? "" } } })));
  const imp = useMutation({
    mutationFn: () => ok<{ imported: number }>(api.POST("/api/admin/helpdesk/calendars/{id}/import", { params: { path: { id: c?.id ?? "" } }, body: { content: importText } })),
    onSuccess: () => {
      setImportText("");
      void special.refetch();
    },
  });
  const h = (d: number) => hours.find((x) => x.day === d);
  const setDay = (d: number, v: Hours | null) => setHours([...hours.filter((x) => x.day !== d), ...(v ? [v] : [])].sort((a, b) => a.day - b.day));
  return (
    <Dialog open onClose={onClose} title={c ? c.name : t("svc.newCalendar")} wide>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <div className="space-y-1">
          <div className="text-sm font-medium">{t("svc.week")}</div>
          {DAYS.map((d) => {
            const x = h(d);
            return (
              <div key={d} className="grid grid-cols-[6rem_auto_7rem_7rem] items-center gap-2 text-sm">
                <span>{dyn(t, `admin.helpdesk.day${d}`)}</span>
                <input type="checkbox" aria-label={dyn(t, `admin.helpdesk.day${d}`)} checked={!!x} onChange={(e) => setDay(d, e.target.checked ? { day: d, start: "09:00", end: "18:00" } : null)} />
                <Input type="time" disabled={!x} value={x?.start ?? ""} onChange={(e) => x && setDay(d, { ...x, start: e.target.value })} />
                <Input type="time" disabled={!x} value={x?.end ?? ""} onChange={(e) => x && setDay(d, { ...x, end: e.target.value })} />
              </div>
            );
          })}
        </div>
        {c && (
          <div className="space-y-1">
            <div className="text-sm font-medium">{t("svc.specialDays")}</div>
            <p className="text-xs text-muted">{t("svc.specialHint")}</p>
            {days?.map((s, i) => (
              <div key={i} className="grid grid-cols-[9rem_6rem_6rem_1fr_auto] gap-2">
                <Input type="date" value={s.day} onChange={(e) => setDays(days.map((x, n) => (n === i ? { ...x, day: e.target.value } : x)))} />
                <Input type="time" value={s.start ?? ""} onChange={(e) => setDays(days.map((x, n) => (n === i ? { ...x, start: e.target.value || null } : x)))} />
                <Input type="time" value={s.end ?? ""} onChange={(e) => setDays(days.map((x, n) => (n === i ? { ...x, end: e.target.value || null } : x)))} />
                <Input value={s.note} placeholder={t("svc.note")} onChange={(e) => setDays(days.map((x, n) => (n === i ? { ...x, note: e.target.value } : x)))} />
                <Button variant="ghost" onClick={() => setDays(days.filter((_, n) => n !== i))}>
                  ×
                </Button>
              </div>
            ))}
            <Button variant="ghost" onClick={() => setDays([...(days ?? []), { day: "", start: null, end: null, note: "" }])}>
              {t("svc.addDay")}
            </Button>
            <Field label={t("svc.import")} hint={t("svc.importHint")}>
              <textarea rows={3} value={importText} onChange={(e) => setImportText(e.target.value)} className={`${area} font-mono text-xs`} />
            </Field>
            <Button disabled={!importText.trim() || imp.isPending} onClick={() => imp.mutate()}>
              {t("svc.importRun")}
            </Button>
            {imp.data && <span className="ml-2 text-sm text-ok">{t("svc.imported", { n: imp.data.imported })}</span>}
            {imp.error && <ErrorBox error={imp.error} />}
          </div>
        )}
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!name.trim() || save.isPending} onDelete={c ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

function PolicyDialog({ value, onClose }: { value: SlaPolicy | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const p = value === "new" ? null : value;
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const cats = useQuery({ queryKey: ["admin-categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories", { params: { query: { all: true } } })) });
  const [f, setF] = useState({
    name: p?.name ?? "",
    site_id: p?.site_id ?? "",
    category_id: p?.category_id ?? "",
    priority: p?.priority ?? "",
    response_minutes: p?.response_minutes?.toString() ?? "",
    resolve_minutes: p?.resolve_minutes?.toString() ?? "",
    sort: p?.sort ?? 0,
    is_active: p?.is_active ?? true,
  });
  const action = useSave(["sla-policies"], onClose);
  const body = {
    ...f,
    site_id: f.site_id || null,
    category_id: f.category_id || null,
    priority: f.priority || null,
    response_minutes: f.response_minutes ? Number(f.response_minutes) : null,
    resolve_minutes: f.resolve_minutes ? Number(f.resolve_minutes) : null,
  };
  const save = action(() => (p ? ok(api.PUT("/api/admin/helpdesk/sla/{id}", { params: { path: { id: p.id } }, body })) : ok(api.POST("/api/admin/helpdesk/sla", { body }))));
  const del = action(() => ok(api.DELETE("/api/admin/helpdesk/sla/{id}", { params: { path: { id: p?.id ?? "" } } })));
  return (
    <Dialog open onClose={onClose} title={p ? p.name : t("svc.newPolicy")} wide>
      <p className="mb-3 text-sm text-muted">{t("svc.policyHint")}</p>
      <div className="grid gap-3 md:grid-cols-2">
        <Field label={t("svc.name")}>
          <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
        </Field>
        <Field label={t("helpdesk.priorityLabel")}>
          <Select value={f.priority} onChange={(e) => setF({ ...f, priority: e.target.value })}>
            <option value="">{t("svc.any")}</option>
            {PRIORITIES.map((x) => (
              <option key={x} value={x}>
                {dyn(t, `helpdesk.priority.${x}`)}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("helpdesk.site")}>
          <Select value={f.site_id} onChange={(e) => setF({ ...f, site_id: e.target.value })}>
            <option value="">{t("svc.any")}</option>
            {sites.data?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("helpdesk.category")}>
          <Select value={f.category_id} onChange={(e) => setF({ ...f, category_id: e.target.value })}>
            <option value="">{t("svc.any")}</option>
            {cats.data?.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("svc.responseMinutes")} hint={t("svc.workMinutesHint")}>
          <Input inputMode="numeric" value={f.response_minutes} onChange={(e) => setF({ ...f, response_minutes: e.target.value.replace(/\D/g, "") })} />
        </Field>
        <Field label={t("svc.resolveMinutes")} hint={t("svc.workMinutesHint")}>
          <Input inputMode="numeric" value={f.resolve_minutes} onChange={(e) => setF({ ...f, resolve_minutes: e.target.value.replace(/\D/g, "") })} />
        </Field>
        <Field label={t("admin.helpdesk.sort")}>
          <Input type="number" value={f.sort} onChange={(e) => setF({ ...f, sort: Number(e.target.value) })} />
        </Field>
        <label className="flex items-center gap-2 pt-6 text-sm">
          <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} /> {t("admin.helpdesk.active")}
        </label>
      </div>
      <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.name.trim() || save.isPending} onDelete={p ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
    </Dialog>
  );
}

function SiteCalendars({ calendars }: { calendars: Calendar[] }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const set = useMutation({
    mutationFn: (v: { site: string; cal: string }) => ok(api.PUT("/api/admin/sites/{id}/calendar", { params: { path: { id: v.site } }, body: { calendar_id: v.cal || null } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["calendars"] }),
  });
  if (!sites.data?.length) return null;
  return (
    <Card className="mt-3 p-4">
      <div className="mb-2 text-sm font-medium">{t("svc.siteCalendars")}</div>
      <p className="mb-2 text-xs text-muted">{t("svc.siteCalendarsHint")}</p>
      <div className="space-y-1">
        {sites.data.map((s) => (
          <div key={s.id} className="grid grid-cols-[1fr_16rem] items-center gap-2 text-sm">
            <span>{s.name}</span>
            <Select aria-label={s.name} value={calendars.find((c) => c.sites.includes(s.name))?.id ?? ""} onChange={(e) => set.mutate({ site: s.id, cal: e.target.value })}>
              <option value="">{t("svc.generalHours")}</option>
              {calendars.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </Select>
          </div>
        ))}
      </div>
      {set.error && <ErrorBox error={set.error} />}
    </Card>
  );
}

const fmtMinutes = (t: (k: never, o?: object) => string, m: number | null | undefined) => (m == null ? "—" : m >= 60 && m % 60 === 0 ? t("svc.hours" as never, { n: m / 60 }) : t("svc.mins" as never, { n: m }));

export function SlaAdminPage() {
  const { t } = useTranslation();
  const tt = t as unknown as (k: never, o?: object) => string;
  const cals = useList("calendars", () => ok<Calendar[]>(api.GET("/api/admin/helpdesk/calendars")));
  const pols = useList("sla-policies", () => ok<SlaPolicy[]>(api.GET("/api/admin/helpdesk/sla")));
  const [cal, setCal] = useState<Calendar | "new" | null>(null);
  const [pol, setPol] = useState<SlaPolicy | "new" | null>(null);
  return (
    <div className="mx-auto max-w-4xl space-y-8">
      {(cals.error || pols.error) && <ErrorBox error={cals.error ?? pols.error} />}
      <div>
        <ListSection
          title={t("svc.calendars")}
          items={cals.data}
          onNew={() => setCal("new")}
          empty={t("svc.noCalendars")}
          row={(c) => (
            <button type="button" className={rowBtn} onClick={() => setCal(c)}>
              <span className="font-medium">{c.name}</span>
              <span className="text-muted">{(c.hours as Hours[]).map((h) => dyn(t, `admin.helpdesk.day${h.day}`)).join(", ")}</span>
              {c.sites.length > 0 && <Badge tone="accent">{c.sites.join(", ")}</Badge>}
            </button>
          )}
        />
        {cals.data && <SiteCalendars calendars={cals.data} />}
      </div>
      <ListSection
        title={t("svc.policies")}
        items={pols.data}
        onNew={() => setPol("new")}
        empty={t("svc.noPolicies")}
        row={(p) => (
          <button type="button" className={rowBtn} onClick={() => setPol(p)}>
            <span className="font-medium">{p.name}</span>
            {p.priority && <Badge>{dyn(t, `helpdesk.priority.${p.priority}`)}</Badge>}
            <span className="text-muted">
              {t("svc.responseShort")} {fmtMinutes(tt, p.response_minutes)} · {t("svc.resolveShort")} {fmtMinutes(tt, p.resolve_minutes)}
            </span>
            {!p.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
          </button>
        )}
      />
      {cal && <CalendarDialog value={cal} onClose={() => setCal(null)} />}
      {pol && <PolicyDialog value={pol} onClose={() => setPol(null)} />}
    </div>
  );
}

// ---------- Группы и правила ----------

function useEngineers() {
  return useQuery({ queryKey: ["engineers", null], queryFn: () => ok<EngineerView[]>(api.GET("/api/helpdesk/engineers")) });
}

function GroupDialog({ value, onClose }: { value: Group | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const g = value === "new" ? null : value;
  const engineers = useEngineers();
  const [f, setF] = useState({ name: g?.name ?? "", description: g?.description ?? "", members: g?.members ?? [], auto_assign: g?.auto_assign ?? false });
  const action = useSave(["groups"], onClose);
  const body = { ...f, description: f.description || null };
  const save = action(() => (g ? ok(api.PUT("/api/admin/helpdesk/groups/{id}", { params: { path: { id: g.id } }, body })) : ok(api.POST("/api/admin/helpdesk/groups", { body }))));
  const del = action(() => ok(api.DELETE("/api/admin/helpdesk/groups/{id}", { params: { path: { id: g?.id ?? "" } } })));
  return (
    <Dialog open onClose={onClose} title={g ? g.name : t("svc.newGroup")}>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
        </Field>
        <Field label={t("svc.description")}>
          <Input value={f.description} onChange={(e) => setF({ ...f, description: e.target.value })} />
        </Field>
        <div>
          <div className="mb-1 text-sm font-medium">{t("svc.members")}</div>
          <div className="max-h-60 space-y-1 overflow-auto">
            {engineers.data?.map((e) => (
              <label key={e.user_id} className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={f.members.includes(e.user_id)} onChange={(x) => setF({ ...f, members: x.target.checked ? [...f.members, e.user_id] : f.members.filter((m) => m !== e.user_id) })} />
                {e.name}
              </label>
            ))}
          </div>
        </div>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.auto_assign} onChange={(e) => setF({ ...f, auto_assign: e.target.checked })} /> {t("svc.autoAssign")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.name.trim() || save.isPending} onDelete={g ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

const TRIGGERS = ["created", "updated", "commented", "sla_warning", "sla_breached"] as const;
const FIELDS = ["text", "title", "description", "comment", "category_id", "site_id", "priority", "status", "channel", "line", "requester_vip", "requester_department", "assignee_id", "group_id"] as const;
const OPS = ["contains", "not_contains", "equals", "not_equals", "is_empty", "not_empty"] as const;
const ACTIONS = ["set_category", "set_priority", "set_site", "set_line", "set_group", "assign", "add_note", "notify", "add_watcher"] as const;

type Cond = { field: string; op: string; value: string };
type Act = { type: string; value: string };

/** Поле значения: для ссылок — выбор из списка, иначе текст. */
function ValueInput({ kind, value, onChange }: { kind: string; value: string; onChange: (v: string) => void }) {
  const { t } = useTranslation();
  const cats = useQuery({ queryKey: ["admin-categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories", { params: { query: { all: true } } })), enabled: kind === "category" });
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")), enabled: kind === "site" });
  const groups = useGroups(kind === "group");
  const engineers = useEngineers();
  const [pick, setPick] = useState(false);
  const opts: { id: string; name: string }[] | null =
    kind === "category"
      ? (cats.data ?? [])
      : kind === "site"
        ? (sites.data ?? [])
        : kind === "group"
          ? (groups.data ?? [])
          : kind === "user"
            ? [...(kind === "user" ? [{ id: "assignee", name: t("svc.toAssignee") }, { id: "superadmins", name: t("svc.toSuperadmins") }] : []), ...(engineers.data ?? []).map((e) => ({ id: e.user_id, name: e.name }))]
            : kind === "engineer"
              ? (engineers.data ?? []).map((e) => ({ id: e.user_id, name: e.name }))
              : kind === "priority"
                ? PRIORITIES.map((p) => ({ id: p, name: dyn(t, `helpdesk.priority.${p}`) }))
                : kind === "line"
                  ? [
                      { id: "l1", name: "L1" },
                      { id: "l2", name: "L2" },
                    ]
                  : null;
  if (kind === "person")
    return pick ? (
      <PersonPicker value={null} onPick={(p: Person | null) => { if (p) onChange(p.id); setPick(false); }} />
    ) : (
      <Button onClick={() => setPick(true)}>{value ? `${value.slice(0, 8)}…` : t("helpdesk.choose")}</Button>
    );
  if (opts)
    return (
      <Select value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{t("helpdesk.choose")}</option>
        {opts.map((o) => (
          <option key={o.id} value={o.id}>
            {o.name}
          </option>
        ))}
      </Select>
    );
  return <Input value={value} onChange={(e) => onChange(e.target.value)} />;
}

const condKind = (f: string) => (f === "category_id" ? "category" : f === "site_id" ? "site" : f === "group_id" ? "group" : f === "assignee_id" ? "engineer" : f === "priority" ? "priority" : f === "line" ? "line" : "text");
const actKind = (a: string) =>
  ({ set_category: "category", set_priority: "priority", set_site: "site", set_line: "line", set_group: "group", assign: "engineer", notify: "user", add_watcher: "person" })[a] ?? "text";

function RuleDialog({ value, onClose }: { value: Rule | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const r = value === "new" ? null : value;
  const [f, setF] = useState({
    name: r?.name ?? "",
    trigger: r?.trigger ?? "created",
    conditions: (r?.conditions ?? []) as Cond[],
    actions: (r?.actions ?? [{ type: "set_priority", value: "" }]) as Act[],
    stop: r?.stop ?? false,
    is_active: r?.is_active ?? true,
    sort: r?.sort ?? 0,
  });
  const action = useSave(["rules"], onClose);
  const save = action(() => (r ? ok(api.PUT("/api/admin/helpdesk/rules/{id}", { params: { path: { id: r.id } }, body: f })) : ok(api.POST("/api/admin/helpdesk/rules", { body: f }))));
  const del = action(() => ok(api.DELETE("/api/admin/helpdesk/rules/{id}", { params: { path: { id: r?.id ?? "" } } })));
  const cond = (i: number, p: Partial<Cond>) => setF({ ...f, conditions: f.conditions.map((c, n) => (n === i ? { ...c, ...p } : c)) });
  const act = (i: number, p: Partial<Act>) => setF({ ...f, actions: f.actions.map((c, n) => (n === i ? { ...c, ...p } : c)) });
  return (
    <Dialog open onClose={onClose} title={r ? r.name : t("svc.newRule")} wide>
      <div className="space-y-3">
        <div className="grid gap-3 md:grid-cols-2">
          <Field label={t("svc.name")}>
            <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
          </Field>
          <Field label={t("svc.trigger")}>
            <Select value={f.trigger} onChange={(e) => setF({ ...f, trigger: e.target.value })}>
              {TRIGGERS.map((x) => (
                <option key={x} value={x}>
                  {dyn(t, `svc.triggers.${x}`)}
                </option>
              ))}
            </Select>
          </Field>
        </div>
        <div>
          <div className="mb-1 text-sm font-medium">{t("svc.conditions")}</div>
          <p className="mb-1 text-xs text-muted">{t("svc.conditionsHint")}</p>
          {f.conditions.map((c, i) => (
            <div key={i} className="mb-1 grid gap-2 md:grid-cols-[1fr_1fr_1.5fr_auto]">
              <Select aria-label={t("svc.field")} value={c.field} onChange={(e) => cond(i, { field: e.target.value, value: "" })}>
                {FIELDS.map((x) => (
                  <option key={x} value={x}>
                    {dyn(t, `svc.fields.${x}`)}
                  </option>
                ))}
              </Select>
              <Select aria-label={t("svc.op")} value={c.op} onChange={(e) => cond(i, { op: e.target.value })}>
                {OPS.map((x) => (
                  <option key={x} value={x}>
                    {dyn(t, `svc.ops.${x}`)}
                  </option>
                ))}
              </Select>
              {c.op === "is_empty" || c.op === "not_empty" ? <span /> : <ValueInput kind={condKind(c.field)} value={c.value} onChange={(v) => cond(i, { value: v })} />}
              <Button variant="ghost" onClick={() => setF({ ...f, conditions: f.conditions.filter((_, n) => n !== i) })}>
                ×
              </Button>
            </div>
          ))}
          <Button variant="ghost" onClick={() => setF({ ...f, conditions: [...f.conditions, { field: "text", op: "contains", value: "" }] })}>
            {t("svc.addCondition")}
          </Button>
        </div>
        <div>
          <div className="mb-1 text-sm font-medium">{t("svc.actions")}</div>
          {f.actions.map((a, i) => (
            <div key={i} className="mb-1 grid gap-2 md:grid-cols-[1fr_2fr_auto]">
              <Select aria-label={t("svc.action")} value={a.type} onChange={(e) => act(i, { type: e.target.value, value: "" })}>
                {ACTIONS.map((x) => (
                  <option key={x} value={x}>
                    {dyn(t, `svc.acts.${x}`)}
                  </option>
                ))}
              </Select>
              <ValueInput kind={actKind(a.type)} value={a.value} onChange={(v) => act(i, { value: v })} />
              <Button variant="ghost" onClick={() => setF({ ...f, actions: f.actions.filter((_, n) => n !== i) })}>
                ×
              </Button>
            </div>
          ))}
          <Button variant="ghost" onClick={() => setF({ ...f, actions: [...f.actions, { type: "add_note", value: "" }] })}>
            {t("svc.addAction")}
          </Button>
        </div>
        <div className="flex flex-wrap gap-4 text-sm">
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} /> {t("admin.helpdesk.active")}
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={f.stop} onChange={(e) => setF({ ...f, stop: e.target.checked })} /> {t("svc.stop")}
          </label>
          <label className="flex items-center gap-2">
            {t("admin.helpdesk.sort")}
            <Input className="w-20" type="number" value={f.sort} onChange={(e) => setF({ ...f, sort: Number(e.target.value) })} />
          </label>
        </div>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.name.trim() || f.actions.length === 0 || save.isPending} onDelete={r ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

export function AutomationAdminPage() {
  const { t } = useTranslation();
  const groups = useGroups();
  const engineers = useEngineers();
  const rules = useList("rules", () => ok<Rule[]>(api.GET("/api/admin/helpdesk/rules")));
  const [g, setG] = useState<Group | "new" | null>(null);
  const [r, setR] = useState<Rule | "new" | null>(null);
  const name = (id: string) => engineers.data?.find((e) => e.user_id === id)?.name ?? "…";
  return (
    <div className="mx-auto max-w-4xl space-y-8">
      {(groups.error || rules.error) && <ErrorBox error={groups.error ?? rules.error} />}
      <ListSection
        title={t("svc.groups")}
        items={groups.data}
        onNew={() => setG("new")}
        empty={t("svc.noGroups")}
        row={(x) => (
          <button type="button" className={rowBtn} onClick={() => setG(x)}>
            <span className="font-medium">{x.name}</span>
            <span className="text-muted">{x.members.map(name).join(", ")}</span>
            {x.auto_assign && <Badge tone="accent">{t("svc.autoAssignShort")}</Badge>}
            <span className="ml-auto text-xs text-muted">{t("svc.openTickets", { n: x.open_tickets })}</span>
          </button>
        )}
      />
      <ListSection
        title={t("svc.rules")}
        items={rules.data}
        onNew={() => setR("new")}
        empty={t("svc.noRules")}
        row={(x) => (
          <button type="button" className={rowBtn} onClick={() => setR(x)}>
            <span className="font-medium">{x.name}</span>
            <Badge>{dyn(t, `svc.triggers.${x.trigger}`)}</Badge>
            <span className="text-muted">{x.actions.map((a) => dyn(t, `svc.acts.${a.type}`)).join(", ")}</span>
            {!x.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
          </button>
        )}
      />
      {g && <GroupDialog value={g} onClose={() => setG(null)} />}
      {r && <RuleDialog value={r} onClose={() => setR(null)} />}
    </div>
  );
}

// ---------- Шаблоны ----------

type Item = { title: string; is_required: boolean };

function TemplateDialog({ value, onClose }: { value: TicketTemplate | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const x = value === "new" ? null : value;
  const cats = useQuery({ queryKey: ["admin-categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories", { params: { query: { all: true } } })) });
  const groups = useGroups();
  const [f, setF] = useState({
    name: x?.name ?? "",
    title: x?.title ?? "",
    description: x?.description ?? "",
    category_id: x?.category_id ?? "",
    group_id: x?.group_id ?? "",
    audience: x?.audience ?? "engineers",
    checklist: ((x?.checklist ?? []) as Item[]).map((i) => ({ title: i.title, is_required: i.is_required ?? true })),
    is_active: x?.is_active ?? true,
    sort: x?.sort ?? 0,
  });
  const action = useSave(["admin-templates", "templates"], onClose);
  const body = { ...f, category_id: f.category_id || null, group_id: f.group_id || null };
  const save = action(() => (x ? ok(api.PUT("/api/admin/helpdesk/templates/{id}", { params: { path: { id: x.id } }, body })) : ok(api.POST("/api/admin/helpdesk/templates", { body }))));
  const del = action(() => ok(api.DELETE("/api/admin/helpdesk/templates/{id}", { params: { path: { id: x?.id ?? "" } } })));
  return (
    <Dialog open onClose={onClose} title={x ? x.name : t("svc.newTemplate")} wide>
      <div className="space-y-3">
        <div className="grid gap-3 md:grid-cols-2">
          <Field label={t("svc.name")}>
            <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
          </Field>
          <Field label={t("svc.audience")}>
            <Select value={f.audience} onChange={(e) => setF({ ...f, audience: e.target.value })}>
              <option value="engineers">{t("svc.audienceEngineers")}</option>
              <option value="all">{t("svc.audienceAll")}</option>
            </Select>
          </Field>
          <Field label={t("svc.ticketTitle")}>
            <Input value={f.title} onChange={(e) => setF({ ...f, title: e.target.value })} />
          </Field>
          <Field label={t("helpdesk.category")}>
            <Select value={f.category_id} onChange={(e) => setF({ ...f, category_id: e.target.value })}>
              <option value="">—</option>
              {cats.data?.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </Select>
          </Field>
          <Field label={t("card.group")}>
            <Select value={f.group_id} onChange={(e) => setF({ ...f, group_id: e.target.value })}>
              <option value="">—</option>
              {groups.data?.map((g) => (
                <option key={g.id} value={g.id}>
                  {g.name}
                </option>
              ))}
            </Select>
          </Field>
          <Field label={t("admin.helpdesk.sort")}>
            <Input type="number" value={f.sort} onChange={(e) => setF({ ...f, sort: Number(e.target.value) })} />
          </Field>
        </div>
        <Field label={t("svc.ticketDescription")}>
          <textarea rows={4} value={f.description} onChange={(e) => setF({ ...f, description: e.target.value })} className={area} />
        </Field>
        <div>
          <div className="mb-1 text-sm font-medium">{t("card.checklist")}</div>
          {f.checklist.map((i, n) => (
            <div key={n} className="mb-1 grid grid-cols-[1fr_auto_auto] items-center gap-2">
              <Input value={i.title} onChange={(e) => setF({ ...f, checklist: f.checklist.map((c, k) => (k === n ? { ...c, title: e.target.value } : c)) })} />
              <label className="flex items-center gap-1 text-xs">
                <input type="checkbox" checked={i.is_required} onChange={(e) => setF({ ...f, checklist: f.checklist.map((c, k) => (k === n ? { ...c, is_required: e.target.checked } : c)) })} />
                {t("admin.helpdesk.required")}
              </label>
              <Button variant="ghost" onClick={() => setF({ ...f, checklist: f.checklist.filter((_, k) => k !== n) })}>
                ×
              </Button>
            </div>
          ))}
          <Button variant="ghost" onClick={() => setF({ ...f, checklist: [...f.checklist, { title: "", is_required: true }] })}>
            {t("svc.addItem")}
          </Button>
        </div>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} /> {t("admin.helpdesk.active")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.name.trim() || save.isPending} onDelete={x ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

const PRESETS = ["daily", "weekly", "monthly", "quarterly"] as const;

function RecurringDialog({ value, templates, onClose }: { value: Recurring | "new"; templates: TicketTemplate[]; onClose: () => void }) {
  const { t } = useTranslation();
  const x = value === "new" ? null : value;
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const [f, setF] = useState({ name: x?.name ?? "", template_id: x?.template_id ?? templates[0]?.id ?? "", schedule: x?.schedule ?? "weekly", site_id: x?.site_id ?? "", is_active: x?.is_active ?? true });
  const [requester, setRequester] = useState<{ id: string; name: string } | null>(x?.requester_id ? { id: x.requester_id, name: "…" } : null);
  const [pick, setPick] = useState(false);
  const action = useSave(["recurring"], onClose);
  const body = { ...f, site_id: f.site_id || null, requester_id: requester?.id ?? null };
  const save = action(() => (x ? ok(api.PUT("/api/admin/helpdesk/recurring/{id}", { params: { path: { id: x.id } }, body })) : ok(api.POST("/api/admin/helpdesk/recurring", { body }))));
  const del = action(() => ok(api.DELETE("/api/admin/helpdesk/recurring/{id}", { params: { path: { id: x?.id ?? "" } } })));
  return (
    <Dialog open onClose={onClose} title={x ? x.name : t("svc.newRecurring")}>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
        </Field>
        <Field label={t("svc.template")}>
          <Select value={f.template_id} onChange={(e) => setF({ ...f, template_id: e.target.value })}>
            {templates.map((tp) => (
              <option key={tp.id} value={tp.id}>
                {tp.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("svc.schedule")} hint={t("svc.scheduleHint")}>
          <div className="flex gap-2">
            <Select className="w-auto" value={PRESETS.includes(f.schedule as (typeof PRESETS)[number]) ? f.schedule : ""} onChange={(e) => setF({ ...f, schedule: e.target.value || "0 9 * * 1" })}>
              {PRESETS.map((p) => (
                <option key={p} value={p}>
                  {dyn(t, `svc.presets.${p}`)}
                </option>
              ))}
              <option value="">cron</option>
            </Select>
            <Input className="font-mono" value={f.schedule} onChange={(e) => setF({ ...f, schedule: e.target.value })} />
          </div>
        </Field>
        <Field label={t("helpdesk.site")}>
          <Select value={f.site_id} onChange={(e) => setF({ ...f, site_id: e.target.value })}>
            <option value="">—</option>
            {sites.data?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("helpdesk.requester")} hint={t("svc.requesterHint")}>
          {pick ? (
            <PersonPicker value={null} onPick={(p: Person | null) => { setRequester(p ? { id: p.id, name: fullName(p) } : null); setPick(false); }} />
          ) : (
            <div className="flex items-center gap-2 text-sm">
              <span className="flex-1">{requester?.name ?? "—"}</span>
              <Button variant="ghost" onClick={() => setPick(true)}>
                {t("common.change")}
              </Button>
              {requester && (
                <Button variant="ghost" onClick={() => setRequester(null)}>
                  ×
                </Button>
              )}
            </div>
          )}
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} /> {t("admin.helpdesk.active")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.name.trim() || !f.template_id || save.isPending} onDelete={x ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

function ResponseDialog({ value, onClose }: { value: ResponseTemplate | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const x = value === "new" ? null : value;
  const [f, setF] = useState({ title: x?.title ?? "", body: x?.body ?? "", shared: x?.shared ?? true });
  const action = useSave(["response-templates"], onClose);
  const save = action(() => (x ? ok(api.PUT("/api/helpdesk/response-templates/{id}", { params: { path: { id: x.id } }, body: f })) : ok(api.POST("/api/helpdesk/response-templates", { body: f }))));
  const del = action(() => ok(api.DELETE("/api/helpdesk/response-templates/{id}", { params: { path: { id: x?.id ?? "" } } })));
  return (
    <Dialog open onClose={onClose} title={x ? x.title : t("svc.newResponse")} wide>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={f.title} onChange={(e) => setF({ ...f, title: e.target.value })} />
        </Field>
        <Field label={t("svc.responseBody")} hint={t("svc.placeholders")}>
          <textarea rows={6} value={f.body} onChange={(e) => setF({ ...f, body: e.target.value })} className={area} />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.shared} onChange={(e) => setF({ ...f, shared: e.target.checked })} /> {t("svc.shared")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.title.trim() || !f.body.trim() || save.isPending} onDelete={x ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

export function TemplatesAdminPage() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const tpls = useList("admin-templates", () => ok<TicketTemplate[]>(api.GET("/api/admin/helpdesk/templates")));
  const recs = useList("recurring", () => ok<Recurring[]>(api.GET("/api/admin/helpdesk/recurring")));
  const resp = useList("response-templates", () => ok<ResponseTemplate[]>(api.GET("/api/helpdesk/response-templates")));
  const [tp, setTp] = useState<TicketTemplate | "new" | null>(null);
  const [rc, setRc] = useState<Recurring | "new" | null>(null);
  const [rs, setRs] = useState<ResponseTemplate | "new" | null>(null);
  const run = useMutation({
    mutationFn: async (id: string) => (await ok(api.POST("/api/admin/helpdesk/recurring/{id}/run", { params: { path: { id } } }))) as { number: number },
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["recurring"] }),
  });
  return (
    <div className="mx-auto max-w-4xl space-y-8">
      {(tpls.error || recs.error || resp.error) && <ErrorBox error={tpls.error ?? recs.error ?? resp.error} />}
      <ListSection
        title={t("svc.templates")}
        items={tpls.data}
        onNew={() => setTp("new")}
        empty={t("svc.noTemplates")}
        row={(x) => (
          <button type="button" className={rowBtn} onClick={() => setTp(x)}>
            <span className="font-medium">{x.name}</span>
            <Badge>{x.audience === "all" ? t("svc.audienceAll") : t("svc.audienceEngineers")}</Badge>
            {x.checklist.length > 0 && <span className="text-muted">{t("svc.items", { n: x.checklist.length })}</span>}
            {!x.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
          </button>
        )}
      />
      <ListSection
        title={t("svc.recurring")}
        items={recs.data}
        onNew={tpls.data?.length ? () => setRc("new") : undefined}
        empty={t("svc.noRecurring")}
        row={(x) => (
          <div className="flex flex-wrap items-center gap-2 px-4 py-2 text-sm">
            <button type="button" className="font-medium hover:underline" onClick={() => setRc(x)}>
              {x.name}
            </button>
            <span className="font-mono text-xs text-muted">{x.schedule}</span>
            {x.is_active && x.next_run_at && <span className="text-xs text-muted">{t("svc.nextRun", { at: formatDate(x.next_run_at, i18n.language) })}</span>}
            {!x.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
            <Button variant="ghost" className="ml-auto" disabled={run.isPending} onClick={() => run.mutate(x.id)}>
              {t("svc.runNow")}
            </Button>
          </div>
        )}
      />
      {run.data && <p className="text-sm text-ok">{t("svc.created", { n: run.data.number })}</p>}
      {run.error && <ErrorBox error={run.error} />}
      <ListSection
        title={t("svc.responses")}
        items={resp.data}
        onNew={() => setRs("new")}
        empty={t("svc.noResponses")}
        row={(x) => (
          <button type="button" className={rowBtn} onClick={() => setRs(x)}>
            <span className="font-medium">{x.title}</span>
            {x.shared ? <Badge tone="accent">{t("svc.shared")}</Badge> : <Badge>{t("svc.personal")}</Badge>}
            <span className="min-w-0 flex-1 truncate text-muted">{x.body}</span>
          </button>
        )}
      />
      {tp && <TemplateDialog value={tp} onClose={() => setTp(null)} />}
      {rc && tpls.data && <RecurringDialog value={rc} templates={tpls.data} onClose={() => setRc(null)} />}
      {rs && <ResponseDialog value={rs} onClose={() => setRs(null)} />}
    </div>
  );
}

// ---------- Типы оборудования ----------

type TypeField = { key: string; label: string; type: string; required: boolean; options: string[] };

function AssetTypeDialog({ value, onClose }: { value: AssetType | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const x = value === "new" ? null : value;
  const [name, setName] = useState(x?.name ?? "");
  const [active, setActive] = useState(x?.is_active ?? true);
  const [showStatus, setShowStatus] = useState(x?.show_status ?? false);
  const [fields, setFields] = useState<TypeField[]>(((x?.fields ?? []) as TypeField[]).map((f) => ({ ...f, required: f.required ?? false, options: f.options ?? [] })));
  const [life, setLife] = useState<Lifecycle>(() => x?.lifecycle ?? { statuses: ["in_use", "in_stock", "repair", "retired"], transitions: [] });
  const action = useSave(["asset-types"], onClose);
  const body = { name, is_active: active, fields, show_status: showStatus };
  const save = action(async () => {
    if (!x) return ok(api.POST("/api/admin/helpdesk/asset-types", { body }));
    await ok(api.PUT("/api/admin/helpdesk/asset-types/{id}", { params: { path: { id: x.id } }, body }));
    return ok(api.PUT("/api/admin/helpdesk/asset-types/{id}/lifecycle", { params: { path: { id: x.id } }, body: life }));
  });
  const edit = (i: number, p: Partial<TypeField>) => setFields(fields.map((f, n) => (n === i ? { ...f, ...p } : f)));
  return (
    <Dialog open onClose={onClose} title={x ? x.name : t("svc.newAssetType")} wide>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <div>
          <div className="mb-1 text-sm font-medium">{t("admin.helpdesk.fields")}</div>
          {fields.map((f, i) => (
            <div key={i} className="mb-2 grid gap-2 md:grid-cols-[1fr_1fr_7rem_1fr_auto]">
              <Input aria-label={t("admin.helpdesk.fieldKey")} placeholder={t("admin.helpdesk.fieldKey")} value={f.key} onChange={(e) => edit(i, { key: e.target.value })} />
              <Input aria-label={t("admin.helpdesk.fieldLabel")} placeholder={t("admin.helpdesk.fieldLabel")} value={f.label} onChange={(e) => edit(i, { label: e.target.value })} />
              <Select aria-label={t("admin.helpdesk.fieldType")} value={f.type} onChange={(e) => edit(i, { type: e.target.value })}>
                <option value="text">{t("admin.helpdesk.typeText")}</option>
                <option value="select">{t("admin.helpdesk.typeSelect")}</option>
                <option value="bool">{t("admin.helpdesk.typeBool")}</option>
              </Select>
              <Input aria-label={t("admin.helpdesk.fieldOptions")} disabled={f.type !== "select"} placeholder={t("admin.helpdesk.fieldOptions")} value={f.options.join(", ")} onChange={(e) => edit(i, { options: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} />
              <Button variant="ghost" onClick={() => setFields(fields.filter((_, n) => n !== i))}>
                ×
              </Button>
            </div>
          ))}
          <Button variant="ghost" onClick={() => setFields([...fields, { key: "", label: "", type: "text", required: false, options: [] }])}>
            {t("admin.helpdesk.addField")}
          </Button>
        </div>
        {x ? <LifecycleEditor value={life} onChange={setLife} /> : <p className="text-xs text-muted">{t("cmdb.lifecycleLater")}</p>}
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" className="mt-1" checked={showStatus} onChange={(e) => setShowStatus(e.target.checked)} />
          <span>
            {t("cmdb.showStatus")}
            <span className="block text-xs text-muted">{t("cmdb.showStatusHint")}</span>
          </span>
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={active} onChange={(e) => setActive(e.target.checked)} /> {t("admin.helpdesk.active")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!name.trim() || save.isPending} error={save.error} />
      </div>
    </Dialog>
  );
}

export function AssetTypesAdminPage() {
  const { t } = useTranslation();
  const types = useList("asset-types", () => ok<AssetType[]>(api.GET("/api/helpdesk/asset-types")));
  const [x, setX] = useState<AssetType | "new" | null>(null);
  return (
    <div className="mx-auto max-w-4xl space-y-8">
      {types.error && <ErrorBox error={types.error} />}
      <ListSection
        title={t("svc.assetTypes")}
        items={types.data}
        onNew={() => setX("new")}
        empty={t("svc.noAssetTypes")}
        row={(a) => (
          <button type="button" className={rowBtn} onClick={() => setX(a)}>
            <span className="font-medium">{a.name}</span>
            <span className="text-muted">{(a.fields as TypeField[]).map((f) => f.label).join(", ")}</span>
            {!a.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
            <span className="ml-auto text-xs text-muted">{a.count}</span>
          </button>
        )}
      />
      {x && <AssetTypeDialog value={x} onClose={() => setX(null)} />}
      <RelationTypesSection />
    </div>
  );
}

function RelationTypeDialog({ value, onClose }: { value: RelationType | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const x = value === "new" ? null : value;
  const [name, setName] = useState(x?.name ?? "");
  const [reverse, setReverse] = useState(x?.reverse_name ?? "");
  const [impact, setImpact] = useState(x?.impact ?? true);
  const [active, setActive] = useState(x?.is_active ?? true);
  const action = useSave(["asset-relation-types"], onClose);
  const body = { name, reverse_name: reverse, impact, is_active: active };
  const save = action(() =>
    x ? ok(api.PUT("/api/admin/helpdesk/asset-relation-types/{id}", { params: { path: { id: x.id } }, body })) : ok(api.POST("/api/admin/helpdesk/asset-relation-types", { body })),
  );
  return (
    <Dialog open onClose={onClose} title={x ? x.name : t("cmdb.newRelationType")}>
      <div className="space-y-3">
        <Field label={t("cmdb.relationName")} hint={t("cmdb.relationNameHint")}>
          <Input value={name} maxLength={100} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label={t("cmdb.reverseName")} hint={t("cmdb.reverseNameHint")}>
          <Input value={reverse} maxLength={100} onChange={(e) => setReverse(e.target.value)} />
        </Field>
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" className="mt-1" checked={impact} onChange={(e) => setImpact(e.target.checked)} />
          <span>
            {t("cmdb.impactFlag")}
            <span className="block text-xs text-muted">{t("cmdb.impactFlagHint")}</span>
          </span>
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={active} onChange={(e) => setActive(e.target.checked)} /> {t("admin.helpdesk.active")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!name.trim() || !reverse.trim() || save.isPending} error={save.error} />
      </div>
    </Dialog>
  );
}

function RelationTypesSection() {
  const { t } = useTranslation();
  const types = useList("asset-relation-types", () => ok<RelationType[]>(api.GET("/api/helpdesk/asset-relation-types")));
  const [x, setX] = useState<RelationType | "new" | null>(null);
  return (
    <>
      {types.error && <ErrorBox error={types.error} />}
      <ListSection
        title={t("cmdb.relationTypes")}
        items={types.data}
        onNew={() => setX("new")}
        empty={t("cmdb.noRelationTypes")}
        row={(r) => (
          <button type="button" className={rowBtn} onClick={() => setX(r)}>
            <span className="font-medium">{r.name}</span>
            <span className="text-muted">↔ {r.reverse_name}</span>
            {r.impact && <Badge tone="accent">{t("cmdb.impactShort")}</Badge>}
            {!r.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
            <span className="ml-auto text-xs text-muted">{r.count}</span>
          </button>
        )}
      />
      {x && <RelationTypeDialog value={x} onClose={() => setX(null)} />}
    </>
  );
}

// ---------- Интеграции: токены API и вебхуки ----------

function TokenDialog({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const me = useMe().data;
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [days, setDays] = useState("365");
  const [perms, setPerms] = useState<string[]>([]);
  const [created, setCreated] = useState<string | null>(null);
  const create = useMutation({
    mutationFn: () => ok<CreatedToken>(api.POST("/api/admin/api-tokens", { body: { name, permissions: perms, days: days ? Number(days) : null } })),
    onSuccess: (r) => {
      setCreated(r.token);
      void qc.invalidateQueries({ queryKey: ["api-tokens"] });
    },
  });
  const mine = (me?.permissions ?? []).filter((p) => p !== "*").sort();
  return (
    <Dialog open onClose={onClose} title={t("svc.newToken")}>
      {created ? (
        <div className="space-y-3">
          <p className="text-sm">{t("svc.tokenOnce")}</p>
          <div className="flex items-center gap-2 rounded-lg border border-line bg-sunken px-3 py-2 font-mono text-xs break-all">
            {created} <CopyButton value={created} label={t("common.copy")} />
          </div>
          <div className="flex justify-end">
            <Button variant="primary" onClick={onClose}>
              {t("common.close")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="space-y-3">
          <p className="text-sm text-muted">{t("svc.tokenHint")}</p>
          <Field label={t("svc.name")}>
            <Input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label={t("svc.tokenDays")}>
            <Input inputMode="numeric" value={days} onChange={(e) => setDays(e.target.value.replace(/\D/g, ""))} />
          </Field>
          <div>
            <div className="mb-1 text-sm font-medium">{t("svc.tokenPerms")}</div>
            <div className="max-h-60 space-y-1 overflow-auto">
              {mine.map((p) => (
                <label key={p} className="flex items-center gap-2 font-mono text-xs">
                  <input type="checkbox" checked={perms.includes(p)} onChange={(e) => setPerms(e.target.checked ? [...perms, p] : perms.filter((x) => x !== p))} />
                  {p}
                </label>
              ))}
            </div>
          </div>
          <Footer onClose={onClose} onSave={() => create.mutate()} disabled={!name.trim() || perms.length === 0 || create.isPending} error={create.error} />
        </div>
      )}
    </Dialog>
  );
}

function WebhookDialog({ value, onClose }: { value: Webhook | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const x = value === "new" ? null : value;
  const [f, setF] = useState({
    name: x?.name ?? "",
    format: x?.format ?? "pult",
    url: x?.format === "telegram" ? "" : (x?.url ?? ""),
    chat_id: x?.chat_id ?? "",
    events: (x?.events ?? []).join(", "),
    secret: "",
    is_active: x?.is_active ?? true,
  });
  const action = useSave(["webhooks"], onClose);
  const tg = f.format === "telegram";
  const body = {
    name: f.name,
    format: f.format,
    url: tg ? "" : f.url,
    chat_id: tg ? f.chat_id : null,
    events: f.events.split(/[\s,]+/).filter(Boolean),
    secret: f.format === "chat" ? null : f.secret || null,
    is_active: f.is_active,
  };
  // Новый секрет нужен при создании и при смене формата (кроме чата — ему секрет не нужен).
  const needSecret = f.format !== "chat" && (!x || x.format !== f.format);
  const secretOk = f.format === "chat" || (f.secret ? (tg ? /^\d{5,15}:[\w-]{30,64}$/.test(f.secret.trim()) : f.secret.trim().length >= 16) : !needSecret);
  const save = action(() => (x ? ok(api.PUT("/api/admin/webhooks/{id}", { params: { path: { id: x.id } }, body })) : ok(api.POST("/api/admin/webhooks", { body }))));
  const del = action(() => ok(api.DELETE("/api/admin/webhooks/{id}", { params: { path: { id: x?.id ?? "" } } })));
  return (
    <Dialog open onClose={onClose} title={x ? x.name : t("svc.newWebhook")}>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
        </Field>
        <Field label={t("svc.format")} hint={t(`svc.formatHint.${f.format as "pult" | "chat" | "telegram"}`)}>
          <Select value={f.format} onChange={(e) => setF({ ...f, format: e.target.value })}>
            {(["pult", "chat", "telegram"] as const).map((k) => (
              <option key={k} value={k}>
                {t(`svc.formats.${k}`)}
              </option>
            ))}
          </Select>
        </Field>
        {tg ? (
          <Field label={t("svc.chatId")} hint={t("svc.chatIdHint")}>
            <Input value={f.chat_id} onChange={(e) => setF({ ...f, chat_id: e.target.value })} placeholder="-1001234567890" />
          </Field>
        ) : (
          <Field label="URL" hint={f.format === "chat" ? t("svc.chatUrlHint") : t("svc.webhookUrlHint")}>
            <Input value={f.url} onChange={(e) => setF({ ...f, url: e.target.value })} placeholder="https://" />
          </Field>
        )}
        <Field label={t("svc.events")} hint={t("svc.eventsHint")}>
          <Input value={f.events} onChange={(e) => setF({ ...f, events: e.target.value })} placeholder="created, status, assigned" />
        </Field>
        {f.format !== "chat" && (
          <Field label={tg ? t("svc.botToken") : t("svc.secret")} hint={needSecret ? (tg ? t("svc.botTokenHint") : t("svc.secretHint")) : t("svc.secretKeep")}>
            <Input type="password" autoComplete="new-password" value={f.secret} onChange={(e) => setF({ ...f, secret: e.target.value })} />
          </Field>
        )}
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} /> {t("admin.helpdesk.active")}
        </label>
        <Footer onClose={onClose} onSave={() => save.mutate(undefined)} disabled={!f.name.trim() || (tg ? !f.chat_id.trim() : !f.url.trim()) || !secretOk || save.isPending} onDelete={x ? () => del.mutate(undefined) : undefined} error={save.error ?? del.error} />
      </div>
    </Dialog>
  );
}

const SEVERITIES = ["info", "warning", "critical"] as const;

/** Источник оповещений мониторинга. Токен показывается один раз — сразу после создания. */
function AlertSourceDialog({ value, onClose }: { value: AlertSource | "new"; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const x = value === "new" ? null : value;
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const cats = useQuery({ queryKey: ["admin-categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories", { params: { query: { all: true } } })) });
  const [f, setF] = useState({
    name: x?.name ?? "",
    create_tickets: x?.create_tickets ?? false,
    min_severity: x?.min_severity ?? "warning",
    category_id: x?.category_id ?? "",
    site_id: x?.site_id ?? "",
    is_active: x?.is_active ?? true,
  });
  const [token, setToken] = useState<string | null>(null);
  const action = useSave(["alert-sources"], onClose);
  const body = { ...f, name: f.name.trim(), category_id: f.category_id || null, site_id: f.site_id || null };
  const create = useMutation({
    mutationFn: () => ok<CreatedSource>(api.POST("/api/admin/monitoring/sources", { body })),
    onSuccess: (r) => {
      setToken(r.token);
      void qc.invalidateQueries({ queryKey: ["alert-sources"] });
    },
  });
  const save = action(() => ok(api.PUT("/api/admin/monitoring/sources/{id}", { params: { path: { id: x?.id ?? "" } }, body })));
  const del = action(() => ok(api.DELETE("/api/admin/monitoring/sources/{id}", { params: { path: { id: x?.id ?? "" } } })));
  const url = `${window.location.origin}/api/helpdesk/monitoring/alerts`;
  if (token)
    return (
      <Dialog open onClose={onClose} title={f.name} wide>
        <div className="space-y-3 text-sm">
          <p>{t("svc.monitoring.tokenOnce")}</p>
          <div className="flex items-center gap-2 rounded-lg border border-line bg-sunken px-3 py-2 font-mono text-xs break-all">
            {token} <CopyButton value={token} label={t("common.copy")} />
          </div>
          <p className="text-muted">{t("svc.monitoring.alertmanager")}</p>
          <pre className="overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-xs">{`receivers:\n  - name: pult\n    webhook_configs:\n      - url: ${url}\n        http_config:\n          authorization:\n            credentials: <token>`}</pre>
          <p className="text-muted">{t("svc.monitoring.generic")}</p>
          <pre className="overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-xs">{`POST ${url}\nAuthorization: Bearer <token>\n\n{"id": "{EVENT.ID}", "title": "{EVENT.NAME}", "status": "{EVENT.VALUE}", "severity": "{EVENT.NSEVERITY}", "host": "{HOST.NAME}"}`}</pre>
          <div className="flex justify-end">
            <Button variant="primary" onClick={onClose}>
              {t("common.close")}
            </Button>
          </div>
        </div>
      </Dialog>
    );
  return (
    <Dialog open onClose={onClose} title={x ? x.name : t("svc.monitoring.new")}>
      <div className="space-y-3">
        <Field label={t("svc.name")}>
          <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} placeholder="Zabbix" />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.create_tickets} onChange={(e) => setF({ ...f, create_tickets: e.target.checked })} /> {t("svc.monitoring.createTickets")}
        </label>
        {f.create_tickets && (
          <>
            <Field label={t("svc.monitoring.minSeverity")}>
              <Select value={f.min_severity} onChange={(e) => setF({ ...f, min_severity: e.target.value })}>
                {SEVERITIES.map((s) => (
                  <option key={s} value={s}>
                    {dyn(t, `svc.monitoring.severity.${s}`)}
                  </option>
                ))}
              </Select>
            </Field>
            <Field label={t("helpdesk.category")}>
              <Select value={f.category_id} onChange={(e) => setF({ ...f, category_id: e.target.value })}>
                <option value="">—</option>
                {cats.data
                  ?.filter((c) => c.is_active)
                  .map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.name}
                    </option>
                  ))}
              </Select>
            </Field>
            <Field label={t("helpdesk.site")}>
              <Select value={f.site_id} onChange={(e) => setF({ ...f, site_id: e.target.value })}>
                <option value="">—</option>
                {sites.data?.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
              </Select>
            </Field>
          </>
        )}
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={f.is_active} onChange={(e) => setF({ ...f, is_active: e.target.checked })} /> {t("admin.helpdesk.active")}
        </label>
        <Footer
          onClose={onClose}
          onSave={() => (x ? save.mutate(undefined) : create.mutate())}
          disabled={!f.name.trim() || save.isPending || create.isPending}
          onDelete={x ? () => window.confirm(t("svc.monitoring.deleteConfirm")) && del.mutate(undefined) : undefined}
          error={create.error ?? save.error ?? del.error}
        />
      </div>
    </Dialog>
  );
}

/** Источники мониторинга и открытые оповещения. */
function MonitoringSection() {
  const { t, i18n } = useTranslation();
  const sources = useList("alert-sources", () => ok<AlertSource[]>(api.GET("/api/admin/monitoring/sources")));
  const alerts = useQuery({ queryKey: ["alerts", "open"], queryFn: () => ok<Alert[]>(api.GET("/api/helpdesk/monitoring/alerts", { params: { query: { open: true } } })) });
  const [edit, setEdit] = useState<AlertSource | "new" | null>(null);
  return (
    <>
      {(sources.error || alerts.error) && <ErrorBox error={sources.error ?? alerts.error} />}
      <ListSection
        title={t("svc.monitoring.title")}
        items={sources.data}
        onNew={() => setEdit("new")}
        empty={t("svc.monitoring.empty")}
        row={(x) => (
          <button type="button" className={rowBtn} onClick={() => setEdit(x)}>
            <span className="font-medium">{x.name}</span>
            {x.open_alerts > 0 && <Badge tone="bad">{t("svc.monitoring.open", { count: x.open_alerts })}</Badge>}
            {x.create_tickets && <Badge tone="accent">{t("svc.monitoring.tickets")}</Badge>}
            {x.last_alert_at && <span className="text-xs text-muted">{t("svc.lastUsed", { at: formatDate(x.last_alert_at, i18n.language) })}</span>}
            {!x.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
          </button>
        )}
      />
      <p className="-mt-6 text-xs text-muted">{t("svc.monitoring.usage")}</p>
      {alerts.data && alerts.data.length > 0 && (
        <Card>
          <ul className="divide-y divide-line">
            {alerts.data.map((a) => (
              <li key={a.id} className="flex flex-wrap items-center gap-2 px-4 py-2 text-sm">
                <Badge tone={a.severity === "critical" ? "bad" : a.severity === "warning" ? "accent" : "muted"}>{dyn(t, `svc.monitoring.severity.${a.severity}`)}</Badge>
                <span className="font-medium">{a.title}</span>
                {a.host && <span className="font-mono text-xs text-muted">{a.host}</span>}
                <span className="text-xs text-muted">{a.source_name}</span>
                {a.ticket_number != null && (
                  <TicketLink number={a.ticket_number} className="text-xs text-accent">
                    #{a.ticket_number}
                  </TicketLink>
                )}
                <span className="ml-auto text-xs text-muted">{formatDate(a.started_at, i18n.language)}</span>
              </li>
            ))}
          </ul>
        </Card>
      )}
      {edit && <AlertSourceDialog value={edit} onClose={() => setEdit(null)} />}
    </>
  );
}

export function IntegrationsPage() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const tokens = useList("api-tokens", () => ok<ApiToken[]>(api.GET("/api/admin/api-tokens")));
  const hooks = useList("webhooks", () => ok<Webhook[]>(api.GET("/api/admin/webhooks")));
  const [newToken, setNewToken] = useState(false);
  const [hook, setHook] = useState<Webhook | "new" | null>(null);
  const revoke = useMutation({
    mutationFn: (id: string) => ok(api.DELETE("/api/admin/api-tokens/{id}", { params: { path: { id } } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["api-tokens"] }),
  });
  return (
    <div className="mx-auto max-w-4xl space-y-8">
      {(tokens.error || hooks.error) && <ErrorBox error={tokens.error ?? hooks.error} />}
      <ListSection
        title={t("svc.tokens")}
        items={tokens.data}
        onNew={() => setNewToken(true)}
        empty={t("svc.noTokens")}
        row={(x) => (
          <div className="flex flex-wrap items-center gap-2 px-4 py-2 text-sm">
            <span className="font-medium">{x.name}</span>
            <span className="font-mono text-xs text-muted">{x.permissions.join(", ")}</span>
            {x.revoked_at ? <Badge tone="bad">{t("svc.revoked")}</Badge> : x.expires_at && <span className="text-xs text-muted">{t("svc.expires", { at: formatDate(x.expires_at, i18n.language) })}</span>}
            {x.last_used_at && <span className="text-xs text-muted">{t("svc.lastUsed", { at: formatDate(x.last_used_at, i18n.language) })}</span>}
            {!x.revoked_at && (
              <Button variant="ghost" className="ml-auto" onClick={() => window.confirm(t("svc.revokeConfirm")) && revoke.mutate(x.id)}>
                {t("svc.revoke")}
              </Button>
            )}
          </div>
        )}
      />
      <p className="-mt-6 text-xs text-muted">{t("svc.tokenUsage")}</p>
      <ListSection
        title={t("svc.webhooks")}
        items={hooks.data}
        onNew={() => setHook("new")}
        empty={t("svc.noWebhooks")}
        row={(x) => (
          <button type="button" className={rowBtn} onClick={() => setHook(x)}>
            <span className="font-medium">{x.name}</span>
            {x.format !== "pult" && <Badge>{t(`svc.formats.${x.format as "chat" | "telegram"}`)}</Badge>}
            <span className="truncate font-mono text-xs text-muted">{x.format === "telegram" ? x.chat_id : x.url}</span>
            {x.last_status != null && <Badge tone={x.last_status >= 200 && x.last_status < 300 ? "ok" : "bad"}>{x.last_status}</Badge>}
            {x.last_error && <span className="truncate text-xs text-bad">{x.last_error}</span>}
            {!x.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
          </button>
        )}
      />
      <p className="-mt-6 text-xs text-muted">{t("svc.webhookUsage")}</p>
      <MonitoringSection />
      {newToken && <TokenDialog onClose={() => setNewToken(false)} />}
      {hook && <WebhookDialog value={hook} onClose={() => setHook(null)} />}
    </div>
  );
}
