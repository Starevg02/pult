// Админка → Helpdesk: категории, рабочее время, отсутствия инженеров.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Absence, type Category, type EngineerView, type HelpdeskSettings } from "../../api/client";
import { formatDate } from "../../format";
import { dyn, PRIORITIES } from "../helpdesk/helpdeskUtil";
import { IMPACTS, URGENCIES } from "../helpdesk/cardUtil";
import { PersonPicker } from "../helpdesk/NewTicketPage";
import type { components } from "../../api/schema";

type VipPerson = components["schemas"]["VipPerson"];

function VipList() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["vip"], queryFn: () => ok<VipPerson[]>(api.GET("/api/admin/helpdesk/vip")) });
  const set = useMutation({
    mutationFn: (v: { id: string; on: boolean }) => ok(api.PUT("/api/admin/helpdesk/vip/{person_id}", { params: { path: { person_id: v.id } }, body: { is_vip: v.on } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["vip"] }),
  });
  return (
    <section>
      <PageHeader title={t("svc.vip")} />
      <Card className="space-y-3 p-4">
        <p className="text-sm text-muted">{t("svc.vipHint")}</p>
        <PersonPicker value={null} onPick={(p) => p && set.mutate({ id: p.id, on: true })} />
        {set.error && <ErrorBox error={set.error} />}
        <ul className="divide-y divide-line text-sm">
          {q.data?.map((v) => (
            <li key={v.id} className="flex items-center gap-3 py-2">
              <span className="font-medium">{v.name}</span>
              <span className="text-muted">{v.department_name}</span>
              <Button variant="ghost" className="ml-auto" onClick={() => set.mutate({ id: v.id, on: false })}>
                {t("common.remove")}
              </Button>
            </li>
          ))}
        </ul>
      </Card>
    </section>
  );
}
import { Badge, Button, Card, Dialog, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";

type FormField = { key: string; label: string; type: string; required: boolean; options: string[] };

function CategoryDialog({ value, all, onClose }: { value: Category | "new"; all: Category[]; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const c = value === "new" ? null : value;
  const [name, setName] = useState(c?.name ?? "");
  const [description, setDescription] = useState(c?.description ?? "");
  const [parent, setParent] = useState(c?.parent_id ?? "");
  const [kind, setKind] = useState(c?.kind ?? "incident");
  const [priority, setPriority] = useState(c?.default_priority ?? "normal");
  const [active, setActive] = useState(c?.is_active ?? true);
  const [sort, setSort] = useState(c?.sort ?? 0);
  const [fields, setFields] = useState<FormField[]>((c?.form_schema ?? []) as unknown as FormField[]);
  const save = useMutation({
    mutationFn: () => {
      const body = { name, description, parent_id: parent || null, kind, default_priority: priority, is_active: active, sort, form_schema: fields, version: c?.version ?? 0 };
      return c ? ok(api.PUT("/api/admin/helpdesk/categories/{id}", { params: { path: { id: c.id } }, body })) : ok(api.POST("/api/admin/helpdesk/categories", { body }));
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["admin-categories"] });
      void qc.invalidateQueries({ queryKey: ["categories"] });
      onClose();
    },
  });
  const edit = (i: number, patch: Partial<FormField>) => setFields(fields.map((f, n) => (n === i ? { ...f, ...patch } : f)));
  return (
    <Dialog open onClose={onClose} title={c ? c.name : t("admin.helpdesk.newCategory")} wide>
      <div className="space-y-3">
        <div className="grid gap-3 md:grid-cols-2">
          <Field label={t("admin.helpdesk.name")}>
            <Input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label={t("admin.helpdesk.parent")}>
            <Select value={parent} onChange={(e) => setParent(e.target.value)}>
              <option value="">—</option>
              {all.filter((x) => x.id !== c?.id).map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
            </Select>
          </Field>
          <Field label={t("admin.helpdesk.kind")}>
            <Select value={kind} onChange={(e) => setKind(e.target.value)}>
              <option value="incident">{t("admin.helpdesk.incident")}</option>
              <option value="request">{t("admin.helpdesk.request")}</option>
            </Select>
          </Field>
          <Field label={t("helpdesk.priorityLabel")}>
            <Select value={priority} onChange={(e) => setPriority(e.target.value)}>
              {["low", "normal", "high", "critical"].map((p) => <option key={p} value={p}>{dyn(t, `helpdesk.priority.${p}`)}</option>)}
            </Select>
          </Field>
          <Field label={t("admin.helpdesk.sort")}>
            <Input type="number" value={sort} onChange={(e) => setSort(Number(e.target.value))} />
          </Field>
          <label className="flex items-center gap-2 pt-6 text-sm">
            <input type="checkbox" checked={active} onChange={(e) => setActive(e.target.checked)} /> {t("admin.helpdesk.active")}
          </label>
        </div>
        <Field label={t("admin.helpdesk.description")} hint={t("admin.helpdesk.descriptionHint")}>
          <Input value={description} maxLength={500} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <div>
          <div className="mb-1 text-sm font-medium">{t("admin.helpdesk.fields")}</div>
          {fields.map((f, i) => (
            <div key={i} className="mb-2 grid gap-2 md:grid-cols-[1fr_1fr_7rem_1fr_auto_auto]">
              <Input aria-label={t("admin.helpdesk.fieldKey")} placeholder={t("admin.helpdesk.fieldKey")} value={f.key} onChange={(e) => edit(i, { key: e.target.value })} />
              <Input aria-label={t("admin.helpdesk.fieldLabel")} placeholder={t("admin.helpdesk.fieldLabel")} value={f.label} onChange={(e) => edit(i, { label: e.target.value })} />
              <Select aria-label={t("admin.helpdesk.fieldType")} value={f.type} onChange={(e) => edit(i, { type: e.target.value })}>
                <option value="text">{t("admin.helpdesk.typeText")}</option>
                <option value="select">{t("admin.helpdesk.typeSelect")}</option>
                <option value="bool">{t("admin.helpdesk.typeBool")}</option>
              </Select>
              <Input aria-label={t("admin.helpdesk.fieldOptions")} disabled={f.type !== "select"} placeholder={t("admin.helpdesk.fieldOptions")} value={f.options.join(", ")} onChange={(e) => edit(i, { options: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} />
              <label className="flex items-center gap-1 text-xs">
                <input type="checkbox" checked={f.required} onChange={(e) => edit(i, { required: e.target.checked })} /> {t("admin.helpdesk.required")}
              </label>
              <Button variant="ghost" aria-label={t("common.remove")} title={t("common.remove")} onClick={() => setFields(fields.filter((_, n) => n !== i))}>
                <span aria-hidden="true">×</span>
              </Button>
            </div>
          ))}
          <Button variant="ghost" onClick={() => setFields([...fields, { key: "", label: "", type: "text", required: false, options: [] }])}>
            {t("admin.helpdesk.addField")}
          </Button>
        </div>
        {save.error && <ErrorBox error={save.error} />}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={save.isPending || !name.trim()} onClick={() => save.mutate()}>{t("common.save")}</Button>
        </div>
      </div>
    </Dialog>
  );
}

export function Categories() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["admin-categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories", { params: { query: { all: true } } })) });
  const [edit, setEdit] = useState<Category | "new" | null>(null);
  if (q.error) return <ErrorBox error={q.error} />;
  if (!q.data) return <Spinner />;
  const by = new Map(q.data.map((c) => [c.id, c]));
  return (
    <section>
      <PageHeader title={t("admin.helpdesk.categories")}>
        <Button variant="primary" onClick={() => setEdit("new")}>{t("admin.helpdesk.newCategory")}</Button>
      </PageHeader>
      <Card>
        <ul className="divide-y divide-line">
          {q.data.map((c) => (
            <li key={c.id}>
              <button type="button" className="flex w-full items-center gap-2 px-4 py-2 text-left text-sm hover:bg-hover" onClick={() => setEdit(c)}>
                <span>{c.parent_id ? `${by.get(c.parent_id)?.name} / ` : ""}{c.name}</span>
                {!c.is_active && <Badge>{t("admin.helpdesk.inactive")}</Badge>}
                {c.code && <Badge tone="accent">{t("admin.helpdesk.system")}</Badge>}
                {(c.form_schema as unknown as unknown[]).length > 0 && <Badge>{t("admin.helpdesk.withFields")}</Badge>}
              </button>
            </li>
          ))}
        </ul>
      </Card>
      {edit && <CategoryDialog value={edit} all={q.data} onClose={() => setEdit(null)} />}
    </section>
  );
}

function SettingsForm({ data }: { data: HelpdeskSettings }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data);
  const [subnets, setSubnets] = useState(data.login_help_subnets.join("\n"));
  const [weekly, setWeekly] = useState(data.weekly_report_to.join("\n"));
  const save = useMutation({
    mutationFn: () =>
      ok<HelpdeskSettings>(
        api.PUT("/api/admin/helpdesk/settings", {
          body: { ...v, login_help_subnets: subnets.split(/[\s,]+/).filter(Boolean), weekly_report_to: weekly.split(/[\s,;]+/).filter(Boolean) },
        }),
      ),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["helpdesk-settings"] }),
  });
  const days = [1, 2, 3, 4, 5, 6, 7];
  return (
    <section>
      <PageHeader title={t("admin.helpdesk.settings")} />
      <Card className="grid gap-4 p-5 md:grid-cols-2">
        <Field label={t("admin.helpdesk.portalUrl")} hint={t("admin.helpdesk.portalUrlHint")}>
          <Input value={v.portal_url} onChange={(e) => setV({ ...v, portal_url: e.target.value })} placeholder="https://pult.corp.example.local" />
        </Field>
        <Field label={t("admin.helpdesk.timezone")}>
          <Input value={v.timezone} onChange={(e) => setV({ ...v, timezone: e.target.value })} />
        </Field>
        <Field label={t("admin.helpdesk.workStart")}>
          <Input type="time" value={v.work_start} onChange={(e) => setV({ ...v, work_start: e.target.value })} />
        </Field>
        <Field label={t("admin.helpdesk.workEnd")}>
          <Input type="time" value={v.work_end} onChange={(e) => setV({ ...v, work_end: e.target.value })} />
        </Field>
        <Field label={t("admin.helpdesk.workDays")}>
          <div className="flex flex-wrap gap-2 text-sm">
            {days.map((d) => (
              <label key={d} className="inline-flex items-center gap-1">
                <input type="checkbox" checked={v.work_days.includes(d)} onChange={(e) => setV({ ...v, work_days: e.target.checked ? [...v.work_days, d].sort() : v.work_days.filter((x) => x !== d) })} />
                {dyn(t, `admin.helpdesk.day${d}`)}
              </label>
            ))}
          </div>
        </Field>
        <Field label={t("admin.helpdesk.emergency")} hint={t("admin.helpdesk.emergencyHint")}>
          <Input value={v.emergency_phone} onChange={(e) => setV({ ...v, emergency_phone: e.target.value })} />
        </Field>
        <Field label={t("admin.helpdesk.autoClose")}>
          <Input type="number" min={1} max={30} value={v.auto_close_days} onChange={(e) => setV({ ...v, auto_close_days: Number(e.target.value) })} />
        </Field>
        <Field label={t("admin.helpdesk.maxFile")}>
          <Input type="number" min={1} max={100} value={v.max_file_mb} onChange={(e) => setV({ ...v, max_file_mb: Number(e.target.value) })} />
        </Field>
        <Field label={t("admin.helpdesk.subnets")} hint={t("admin.helpdesk.subnetsHint")}>
          <textarea rows={3} value={subnets} onChange={(e) => setSubnets(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs" />
        </Field>
        <h3 className="font-medium md:col-span-2">{t("svc.serviceSettings")}</h3>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={v.sla_pause_waiting} onChange={(e) => setV({ ...v, sla_pause_waiting: e.target.checked })} /> {t("svc.pauseWaiting")}
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={v.vip_bump} onChange={(e) => setV({ ...v, vip_bump: e.target.checked })} /> {t("svc.vipBump")}
        </label>
        <Field label={t("svc.remindDays")} hint={t("svc.remindHint")}>
          <Input type="number" min={0} max={30} value={v.waiting_remind_days} onChange={(e) => setV({ ...v, waiting_remind_days: Number(e.target.value) })} />
        </Field>
        <Field label={t("svc.resolveDays")} hint={t("svc.resolveDaysHint")}>
          <Input type="number" min={0} max={30} value={v.waiting_resolve_days} onChange={(e) => setV({ ...v, waiting_resolve_days: Number(e.target.value) })} />
        </Field>
        <Field label={t("svc.weeklyTo")} hint={t("svc.weeklyHint")}>
          <textarea rows={2} value={weekly} onChange={(e) => setWeekly(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs" />
        </Field>
        <div className="md:col-span-2">
          <div className="mb-1 text-sm font-medium">{t("svc.matrix")}</div>
          <p className="mb-2 text-xs text-muted">{t("svc.matrixHint")}</p>
          <table className="text-sm">
            <thead>
              <tr>
                <th className="pr-2 text-left text-xs font-normal text-muted">{t("card.impact")} \ {t("card.urgency")}</th>
                {URGENCIES.map((u) => <th key={u} className="px-1 text-xs font-normal text-muted">{dyn(t, `card.urgencies.${u}`)}</th>)}
              </tr>
            </thead>
            <tbody>
              {IMPACTS.map((imp, i) => (
                <tr key={imp}>
                  <td className="pr-2 text-xs text-muted">{dyn(t, `card.impacts.${imp}`)}</td>
                  {URGENCIES.map((u, j) => (
                    <td key={u} className="p-1">
                      <Select
                        aria-label={`${imp} ${u}`}
                        value={v.priority_matrix[i]?.[j] ?? "normal"}
                        onChange={(e) => setV({ ...v, priority_matrix: v.priority_matrix.map((row, a) => (a === i ? row.map((c, b) => (b === j ? e.target.value : c)) : row)) })}
                      >
                        {PRIORITIES.map((p) => <option key={p} value={p}>{dyn(t, `helpdesk.priority.${p}`)}</option>)}
                      </Select>
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <div className="md:col-span-2">
          <div className="mb-1 text-sm font-medium">{t("svc.closeCodes")}</div>
          <p className="mb-2 text-xs text-muted">{t("svc.closeCodesHint")}</p>
          {v.close_codes.map((c, i) => (
            <div key={i} className="mb-1 grid grid-cols-[10rem_1fr_auto] gap-2">
              <Input aria-label={t("svc.code")} className="font-mono" value={c.code} disabled={c.code === "no_response" || c.code === "duplicate"} onChange={(e) => setV({ ...v, close_codes: v.close_codes.map((x, n) => (n === i ? { ...x, code: e.target.value } : x)) })} />
              <Input aria-label={t("svc.label")} value={c.label} onChange={(e) => setV({ ...v, close_codes: v.close_codes.map((x, n) => (n === i ? { ...x, label: e.target.value } : x)) })} />
              <Button variant="ghost" disabled={c.code === "no_response" || c.code === "duplicate"} aria-label={t("common.remove")} title={t("common.remove")} onClick={() => setV({ ...v, close_codes: v.close_codes.filter((_, n) => n !== i) })}><span aria-hidden="true">×</span></Button>
            </div>
          ))}
          <Button variant="ghost" onClick={() => setV({ ...v, close_codes: [...v.close_codes, { code: "", label: "" }] })}>
            {t("svc.addCode")}
          </Button>
        </div>
        <div className="md:col-span-2">
          {save.error && <ErrorBox error={save.error} />}
          {save.isSuccess && <p className="mb-2 text-sm text-ok">{t("admin.helpdesk.saved")}</p>}
          <Button variant="primary" disabled={save.isPending} onClick={() => save.mutate()}>{t("common.save")}</Button>
        </div>
      </Card>
    </section>
  );
}

function Settings() {
  const q = useQuery({ queryKey: ["helpdesk-settings"], queryFn: () => ok<HelpdeskSettings>(api.GET("/api/admin/helpdesk/settings")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (!q.data) return <Spinner />;
  // Без key: фоновое обновление запроса не сбрасывает несохранённый ввод и отметку «Сохранено».
  return <SettingsForm data={q.data} />;
}

function Absences() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["absences"], queryFn: () => ok<Absence[]>(api.GET("/api/admin/helpdesk/absences")) });
  const engineers = useQuery({ queryKey: ["engineers", null], queryFn: () => ok<EngineerView[]>(api.GET("/api/helpdesk/engineers")) });
  const [user, setUser] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [reason, setReason] = useState("");
  const refresh = () => void qc.invalidateQueries({ queryKey: ["absences"] });
  const add = useMutation({
    mutationFn: () => ok(api.POST("/api/admin/helpdesk/absences", { body: { user_id: user, starts_at: new Date(from).toISOString(), ends_at: new Date(to).toISOString(), reason: reason || null } })),
    onSuccess: () => {
      setReason("");
      refresh();
    },
  });
  const del = useMutation({ mutationFn: (id: string) => ok(api.DELETE("/api/admin/helpdesk/absences/{id}", { params: { path: { id } } })), onSuccess: refresh });
  return (
    <section>
      <PageHeader title={t("admin.helpdesk.absences")} />
      <Card className="p-4">
        <div className="grid gap-2 md:grid-cols-[1fr_auto_auto_1fr_auto]">
          <Select aria-label={t("helpdesk.engineer")} value={user} onChange={(e) => setUser(e.target.value)}>
            <option value="">{t("helpdesk.engineer")}</option>
            {engineers.data?.map((e) => <option key={e.user_id} value={e.user_id}>{e.name}</option>)}
          </Select>
          <Input aria-label={t("admin.helpdesk.from")} type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
          <Input aria-label={t("admin.helpdesk.to")} type="datetime-local" value={to} onChange={(e) => setTo(e.target.value)} />
          <Input aria-label={t("admin.helpdesk.reason")} placeholder={t("admin.helpdesk.reason")} value={reason} onChange={(e) => setReason(e.target.value)} />
          <Button variant="primary" disabled={!user || !from || !to || add.isPending} onClick={() => add.mutate()}>{t("common.add")}</Button>
        </div>
        {add.error && <ErrorBox error={add.error} />}
        <ul className="mt-3 divide-y divide-line text-sm">
          {q.data?.map((a) => (
            <li key={a.id} className="flex items-center gap-3 py-2">
              <span className="font-medium">{a.name}</span>
              <span className="text-muted">{formatDate(a.starts_at, i18n.language)} — {formatDate(a.ends_at, i18n.language)}</span>
              <span className="text-muted">{a.reason}</span>
              <Button variant="ghost" className="ml-auto" disabled={del.isPending} onClick={() => window.confirm(t("common.confirmDelete")) && del.mutate(a.id)}>{t("common.delete")}</Button>
            </li>
          ))}
        </ul>
      </Card>
    </section>
  );
}

export function HelpdeskAdminPage() {
  return (
    <div className="mx-auto max-w-4xl space-y-8">
      <Categories />
      <Settings />
      <VipList />
      <Absences />
    </div>
  );
}
