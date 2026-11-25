// Блоки карточки заявки: SLA, исполнитель, наблюдатели, чек-лист, объекты, связи, оценка.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type EngineerView, type FoundObject, type Person, type ResponseTemplate, type TicketDetail } from "../../api/client";
import { dueText, IMPACTS, OBJECT_KINDS, URGENCIES, useCloseCodes, useGroups, usePersonAssets, useTicketMutation } from "./cardUtil";
import { formatDate } from "../../format";
import { Badge, Button, ErrorBox, Input, Select } from "../../ui";
import { dyn, type Tone } from "./helpdeskUtil";
import { PersonPicker } from "./NewTicketPage";
import { StatusBadge, TicketLink } from "./parts";
import { ChecklistRunDialog } from "./directory";
import { useModule } from "../../session";

const slaTone: Record<string, Tone> = { ok: "ok", warning: "accent", breached: "bad", paused: "muted", met: "ok" };

export function SlaBadge({ state }: { state?: string | null }) {
  const { t } = useTranslation();
  if (!state) return null;
  return <Badge tone={slaTone[state] ?? "muted"}>{dyn(t, `card.sla.${state}`)}</Badge>;
}

function Section({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <div className="mt-3 border-t border-line pt-2 text-sm">
      <div className="mb-1 flex items-center gap-2">
        <span className="font-medium">{title}</span>
        <span className="ml-auto">{action}</span>
      </div>
      {children}
    </div>
  );
}

export function SlaPanel({ d, lang }: { d: TicketDetail; lang: string }) {
  const { t } = useTranslation();
  const x = d.ticket;
  const [, tick] = useState(0);
  useEffect(() => {
    const h = setInterval(() => tick((v) => v + 1), 60_000);
    return () => clearInterval(h);
  }, []);
  if (!x.sla_state) return null;
  const tt = t as unknown as (k: never, o?: object) => string;
  return (
    <Section title={t("card.slaTitle")} action={<SlaBadge state={x.sla_state} />}>
      {x.sla_policy_name && <div className="text-xs text-muted">{x.sla_policy_name}</div>}
      {x.response_due_at && !x.first_response_at && (
        <div className="flex gap-2">
          <span className="text-muted">{t("card.responseDue")}</span>
          <span className="ml-auto text-right">
            {formatDate(x.response_due_at, lang)}
            <br />
            <span className="text-xs text-muted">{x.sla_paused ? t("card.paused") : dueText(tt, x.response_due_at)}</span>
          </span>
        </div>
      )}
      {x.resolve_due_at && (
        <div className="flex gap-2">
          <span className="text-muted">{t("card.resolveDue")}</span>
          <span className="ml-auto text-right">
            {formatDate(x.resolve_due_at, lang)}
            {!x.resolved_at && (
              <>
                <br />
                <span className="text-xs text-muted">{x.sla_paused ? t("card.paused") : dueText(tt, x.resolve_due_at)}</span>
              </>
            )}
          </span>
        </div>
      )}
    </Section>
  );
}

/** Исполнитель: выбрать инженера или вернуть в очередь. */
export function AssigneeControl({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const x = d.ticket;
  const [editing, setEditing] = useState(false);
  const engineers = useQuery({
    queryKey: ["engineers", x.site_id],
    queryFn: () => ok<EngineerView[]>(api.GET("/api/helpdesk/engineers", { params: { query: { site_id: x.site_id ?? undefined } } })),
    enabled: editing,
  });
  const assign = useTicketMutation(x.number, (id: string | null) => ok(api.POST("/api/helpdesk/tickets/{number}/assign", { params: { path: { number: x.number } }, body: { assignee_id: id } })), () => setEditing(false));
  const open = !["resolved", "closed", "canceled"].includes(x.status);
  if (!d.can_work || !open) return <>{x.assignee_name ?? t("helpdesk.noAssignee")}</>;
  if (!editing)
    return (
      <button type="button" className="text-left hover:underline" onClick={() => setEditing(true)} title={t("card.changeAssignee")} aria-label={`${t("card.changeAssignee")}: ${x.assignee_name ?? t("helpdesk.noAssignee")}`}>
        {x.assignee_name ?? t("helpdesk.noAssignee")} <span aria-hidden="true" className="text-xs text-accent">✎</span>
      </button>
    );
  return (
    <div className="space-y-1">
      <Select
        autoFocus
        aria-label={t("helpdesk.assignee")}
        value={x.assignee_id ?? ""}
        onChange={(e) => assign.mutate(e.target.value || null)}
        onBlur={() => !assign.isPending && setEditing(false)}
      >
        <option value="">{t("card.toQueue")}</option>
        {x.assignee_id && !engineers.data?.some((e) => e.user_id === x.assignee_id) && <option value={x.assignee_id}>{x.assignee_name}</option>}
        {engineers.data?.map((e) => (
          <option key={e.user_id} value={e.user_id}>
            {e.name} · {e.load}
            {e.absent ? ` · ${t("helpdesk.absent")}` : ""}
          </option>
        ))}
      </Select>
      {assign.error && <ErrorBox error={assign.error} />}
    </div>
  );
}

export function GroupControl({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const x = d.ticket;
  const qc = useQueryClient();
  const groups = useGroups(d.can_work);
  const update = useMutation({
    mutationFn: (g: string) => ok(api.PUT("/api/helpdesk/tickets/{number}", { params: { path: { number: x.number } }, body: g ? { version: x.version, group_id: g } : { version: x.version, clear_group: true } })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["ticket", x.number] }),
  });
  if (!d.can_work) return <>{x.group_name ?? "—"}</>;
  return (
    <>
      <Select aria-label={t("card.group")} value={x.group_id ?? ""} onChange={(e) => update.mutate(e.target.value)}>
        <option value="">—</option>
        {groups.data?.map((g) => (
          <option key={g.id} value={g.id}>
            {g.name}
          </option>
        ))}
      </Select>
      {update.error && <ErrorBox error={update.error} />}
    </>
  );
}

export function ImpactUrgency({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const x = d.ticket;
  const qc = useQueryClient();
  const update = useMutation({
    mutationFn: (b: { impact?: string; urgency?: string }) => ok(api.PUT("/api/helpdesk/tickets/{number}", { params: { path: { number: x.number } }, body: { version: x.version, ...b } })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["ticket", x.number] }),
  });
  return (
    <div className="grid grid-cols-2 gap-1">
      <Select aria-label={t("card.impact")} title={t("card.impact")} value={x.impact} onChange={(e) => update.mutate({ impact: e.target.value })}>
        {IMPACTS.map((i) => (
          <option key={i} value={i}>
            {dyn(t, `card.impacts.${i}`)}
          </option>
        ))}
      </Select>
      <Select aria-label={t("card.urgency")} title={t("card.urgency")} value={x.urgency} onChange={(e) => update.mutate({ urgency: e.target.value })}>
        {URGENCIES.map((u) => (
          <option key={u} value={u}>
            {dyn(t, `card.urgencies.${u}`)}
          </option>
        ))}
      </Select>
      {update.error && (
        <div className="col-span-2">
          <ErrorBox error={update.error} />
        </div>
      )}
    </div>
  );
}

export function Watchers({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const n = d.ticket.number;
  const [adding, setAdding] = useState(false);
  const [person, setPerson] = useState<Person | null>(null);
  const add = useTicketMutation(n, (p: Person) => ok(api.POST("/api/helpdesk/tickets/{number}/watchers", { params: { path: { number: n } }, body: { person_id: p.id } })), () => {
    setAdding(false);
    setPerson(null);
  });
  const remove = useTicketMutation(n, (id: string) => ok(api.DELETE("/api/helpdesk/tickets/{number}/watchers/{person_id}", { params: { path: { number: n, person_id: id } } })));
  const canAdd = d.can_work || d.is_requester;
  if (d.watchers.length === 0 && !canAdd) return null;
  return (
    <Section title={t("card.watchers")} action={canAdd && !adding && <Button variant="ghost" onClick={() => setAdding(true)}>+ {t("common.add")}</Button>}>
      {d.watchers.length === 0 && !adding && <div className="text-xs text-muted">{t("card.noWatchers")}</div>}
      <ul className="space-y-1">
        {d.watchers.map((w) => (
          <li key={w.person_id} className="flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate">{w.name}</span>
            {(d.can_work || d.is_requester || d.is_watcher) && (
              <button type="button" aria-label={t("common.remove")} className="text-xs text-muted hover:text-bad" onClick={() => remove.mutate(w.person_id)}>
                ✕
              </button>
            )}
          </li>
        ))}
      </ul>
      {adding && (
        <div className="mt-2 space-y-2">
          <PersonPicker value={person} onPick={(p) => (p ? add.mutate(p) : setPerson(null))} />
          <Button variant="ghost" onClick={() => setAdding(false)}>
            {t("common.cancel")}
          </Button>
        </div>
      )}
      {(add.error || remove.error) && <ErrorBox error={add.error ?? remove.error} />}
    </Section>
  );
}

export function Checklist({ d }: { d: TicketDetail }) {
  const { t, i18n } = useTranslation();
  const n = d.ticket.number;
  const [title, setTitle] = useState("");
  const path = (id: string) => ({ params: { path: { number: n, id } } });
  const add = useTicketMutation(n, () => ok(api.POST("/api/helpdesk/tickets/{number}/checklist", { params: { path: { number: n } }, body: { items: [{ title }] } })), () => setTitle(""));
  const toggle = useTicketMutation(n, (v: { id: string; done: boolean }) => ok(api.PUT("/api/helpdesk/tickets/{number}/checklist/{id}", { ...path(v.id), body: { done: v.done } })));
  const remove = useTicketMutation(n, (id: string) => ok(api.DELETE("/api/helpdesk/tickets/{number}/checklist/{id}", path(id))));
  const items = d.checklist;
  const [running, setRunning] = useState<{ id: string; action: string; title: string } | null>(null);
  // Действия чек-листа выполняются в AD: без модуля «Изменения в AD» — только отметка вручную.
  const directory = useModule("directory");
  if (items.length === 0 && !d.can_work) return null;
  const done = items.filter((i) => i.done_at).length;
  const open = !["resolved", "closed", "canceled"].includes(d.ticket.status);
  return (
    <Section title={t("card.checklist")} action={items.length > 0 && <span className="text-xs text-muted">{done}/{items.length}</span>}>
      <ul className="space-y-1">
        {items.map((i) => (
          <li key={i.id} className="flex items-start gap-2">
            <input
              type="checkbox"
              className="mt-1"
              checked={!!i.done_at}
              disabled={!d.can_work || !open || toggle.isPending}
              onChange={(e) => toggle.mutate({ id: i.id, done: e.target.checked })}
              aria-label={i.title}
            />
            <span className="min-w-0 flex-1">
              <span className={i.done_at ? "text-muted line-through" : ""}>{i.title}</span>
              {!i.is_required && <span className="text-xs text-muted"> · {t("card.optional")}</span>}
              {(i.assignee_name || i.done_by_name) && (
                <span className="block text-xs text-muted">
                  {i.assignee_name}
                  {i.done_at && ` · ${i.done_by_name ?? ""} ${formatDate(i.done_at, i18n.language)}`}
                </span>
              )}
            </span>
            {d.can_work && directory && open && i.action && !i.done_at && (
              <Button className="px-2 py-0.5 text-xs" onClick={() => setRunning({ id: i.id, action: i.action ?? "", title: i.title })}>
                {t("directory.run")}
              </Button>
            )}
            {d.can_work && open && (
              <button type="button" aria-label={t("common.remove")} className="text-xs text-muted hover:text-bad" onClick={() => remove.mutate(i.id)}>
                ✕
              </button>
            )}
          </li>
        ))}
      </ul>
      {d.can_work && open && (
        <form
          className="mt-2 flex gap-1"
          onSubmit={(e) => {
            e.preventDefault();
            if (title.trim()) add.mutate(undefined);
          }}
        >
          <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t("card.newTask")} />
          <Button type="submit" disabled={!title.trim() || add.isPending}>
            +
          </Button>
        </form>
      )}
      {(add.error || toggle.error || remove.error) && <ErrorBox error={add.error ?? toggle.error ?? remove.error} />}
      {running && <ChecklistRunDialog number={n} item={running.id} action={running.action} title={running.title} onClose={() => setRunning(null)} />}
    </Section>
  );
}

/** Поиск объекта для привязки к заявке. */
export function ObjectSearch({ onPick, kinds }: { onPick: (o: FoundObject) => void; kinds?: string }) {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  const [kind, setKind] = useState(kinds ?? "");
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const found = useQuery({
    queryKey: ["objects", needle, kind],
    queryFn: () => ok<FoundObject[]>(api.GET("/api/helpdesk/objects", { params: { query: { q: needle, kind: kind || undefined } } })),
    enabled: needle.length > 1,
  });
  return (
    <div className="space-y-1">
      <div className="flex gap-1">
        <Input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("card.objectSearch")} />
        {!kinds && (
          <Select aria-label={t("card.objectKind")} className="w-auto" value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="">{t("card.allKinds")}</option>
            {OBJECT_KINDS.map((k) => (
              <option key={k} value={k}>
                {dyn(t, `card.kinds.${k}`)}
              </option>
            ))}
          </Select>
        )}
      </div>
      {found.data && (
        <ul className="max-h-60 divide-y divide-line overflow-auto rounded-lg border border-line">
          {found.data.length === 0 && <li className="px-3 py-2 text-xs text-muted">{t("card.nothingFound")}</li>}
          {found.data.map((o) => (
            <li key={`${o.kind}:${o.id}`}>
              <button type="button" className="w-full px-3 py-2 text-left text-sm hover:bg-hover" onClick={() => onPick(o)}>
                <Badge>{dyn(t, `card.kinds.${o.kind}`)}</Badge> {o.label} {o.detail && <span className="text-xs text-muted">· {o.detail}</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function ObjectLink({ kind, id, children }: { kind: string; id: string; children: ReactNode }) {
  return (
    <Link to="/objects/$kind/$id" params={{ kind, id }} className="text-accent hover:underline">
      {children}
    </Link>
  );
}

export function Objects({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const n = d.ticket.number;
  const [adding, setAdding] = useState(false);
  const add = useTicketMutation(n, (o: FoundObject) => ok(api.POST("/api/helpdesk/tickets/{number}/objects", { params: { path: { number: n } }, body: { kind: o.kind, object_id: o.id } })), () => setAdding(false));
  const remove = useTicketMutation(n, (o: { kind: string; id: string }) => ok(api.DELETE("/api/helpdesk/tickets/{number}/objects/{kind}/{id}", { params: { path: { number: n, kind: o.kind, id: o.id } } })));
  // Оборудование заявителя — подсказка при привязке.
  const requesterAssets = usePersonAssets(d.can_work && adding ? d.ticket.requester_person_id : null);
  const suggested = (requesterAssets.data ?? []).filter((a) => !d.objects.some((o) => o.kind === "asset" && o.object_id === a.id));
  if (!d.can_work) {
    // Заявителю — оборудование по его заявке, без изменения.
    if (d.objects.length === 0) return null;
    return (
      <Section title={t("helpdesk.equipment")}>
        <ul className="space-y-1">
          {d.objects.map((o) => (
            <li key={`${o.kind}:${o.object_id}`} className="text-sm">
              {o.label}
              {o.detail && <span className="block text-xs text-muted">{o.detail}</span>}
            </li>
          ))}
        </ul>
      </Section>
    );
  }
  return (
    <Section title={t("card.objects")} action={!adding && <Button variant="ghost" onClick={() => setAdding(true)}>+ {t("common.add")}</Button>}>
      {d.objects.length === 0 && !adding && <div className="text-xs text-muted">{t("card.noObjects")}</div>}
      <ul className="space-y-1">
        {d.objects.map((o) => (
          <li key={`${o.kind}:${o.object_id}`} className="flex items-start gap-2">
            <span className="min-w-0 flex-1">
              <Badge>{dyn(t, `card.kinds.${o.kind}`)}</Badge>{" "}
              <ObjectLink kind={o.kind} id={o.object_id}>
                {o.label}
              </ObjectLink>
              {o.detail && <span className="block text-xs text-muted">{o.detail}</span>}
            </span>
            <button type="button" aria-label={t("common.remove")} className="text-xs text-muted hover:text-bad" onClick={() => remove.mutate({ kind: o.kind, id: o.object_id })}>
              ✕
            </button>
          </li>
        ))}
      </ul>
      {adding && (
        <div className="mt-2 space-y-1">
          {suggested.length > 0 && (
            <div>
              <div className="mb-1 text-xs text-muted">{t("helpdesk.equipmentOf")}</div>
              <ul className="space-y-1">
                {suggested.map((a) => (
                  <li key={a.id}>
                    <button type="button" className="w-full rounded-lg px-2 py-1 text-left text-sm hover:bg-hover" onClick={() => add.mutate(a)}>
                      + {a.label} {a.detail && <span className="text-xs text-muted">· {a.detail}</span>}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <ObjectSearch onPick={(o) => add.mutate(o)} />
          <Button variant="ghost" onClick={() => setAdding(false)}>
            {t("common.cancel")}
          </Button>
        </div>
      )}
      {(add.error || remove.error) && <ErrorBox error={add.error ?? remove.error} />}
    </Section>
  );
}

export function Links({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const n = d.ticket.number;
  const [adding, setAdding] = useState(false);
  const [other, setOther] = useState("");
  const [kind, setKind] = useState("related");
  const add = useTicketMutation(n, () => ok(api.POST("/api/helpdesk/tickets/{number}/links", { params: { path: { number: n } }, body: { number: Number(other), kind } })), () => {
    setAdding(false);
    setOther("");
  });
  const remove = useTicketMutation(n, (o: number) => ok(api.DELETE("/api/helpdesk/tickets/{number}/links/{other}", { params: { path: { number: n, other: o } } })));
  if (!d.can_work) return null;
  return (
    <Section title={t("card.links")} action={!adding && <Button variant="ghost" onClick={() => setAdding(true)}>+ {t("common.add")}</Button>}>
      <ul className="space-y-1">
        {d.links.map((l) => (
          <li key={l.number} className="flex items-center gap-2">
            <TicketLink number={l.number} className="font-mono text-xs text-accent">
              #{l.number}
            </TicketLink>
            <span className="min-w-0 flex-1 truncate" title={l.title}>
              {l.title}
            </span>
            <span className="text-xs text-muted">{dyn(t, `card.linkKinds.${l.kind}`)}</span>
            <StatusBadge status={l.status} />
            <button type="button" aria-label={t("common.remove")} className="text-xs text-muted hover:text-bad" onClick={() => remove.mutate(l.number)}>
              ✕
            </button>
          </li>
        ))}
      </ul>
      {adding && (
        <form
          className="mt-2 flex gap-1"
          onSubmit={(e) => {
            e.preventDefault();
            if (other) add.mutate(undefined);
          }}
        >
          <Input autoFocus inputMode="numeric" placeholder={t("card.ticketNumber")} value={other} onChange={(e) => setOther(e.target.value.replace(/\D/g, ""))} />
          <Select aria-label={t("card.linkKind")} className="w-auto" value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="related">{t("card.linkKinds.related")}</option>
            <option value="follow_up">{t("card.linkKinds.follow_up")}</option>
          </Select>
          <Button type="submit" disabled={!other || add.isPending}>
            {t("common.add")}
          </Button>
        </form>
      )}
      {(add.error || remove.error) && <ErrorBox error={add.error ?? remove.error} />}
    </Section>
  );
}

const SURVEY = ["speed", "quality", "courtesy"] as const;
type SurveyKey = (typeof SURVEY)[number];

/** Балл 1–5 подробного опроса; повторный щелчок снимает выбор. */
function ScoreRow({ label, value, onChange }: { label: string; value: number | null; onChange: (v: number | null) => void }) {
  return (
    <div role="radiogroup" aria-label={label} className="flex flex-wrap items-center gap-1">
      <span className="w-40 text-muted">{label}</span>
      {[1, 2, 3, 4, 5].map((n) => (
        <button
          key={n}
          type="button"
          role="radio"
          aria-checked={value === n}
          onClick={() => onChange(value === n ? null : n)}
          className={`h-8 w-8 rounded-lg border text-sm ${value !== null && n <= value ? "border-accent bg-accent text-paper" : "border-control bg-paper"}`}
        >
          {n}
        </button>
      ))}
    </div>
  );
}

/** Оценка решения заявителем: 👍 / 👎, по желанию баллы 1–5 и комментарий. */
export function Rating({ d }: { d: TicketDetail }) {
  const { t } = useTranslation();
  const x = d.ticket;
  const [value, setValue] = useState<"up" | "down" | null>(null);
  const [comment, setComment] = useState("");
  const [scores, setScores] = useState<Record<SurveyKey, number | null>>({ speed: null, quality: null, courtesy: null });
  const rate = useTicketMutation(
    x.number,
    (v: "up" | "down") =>
      ok(api.POST("/api/helpdesk/tickets/{number}/rating", { params: { path: { number: x.number } }, body: { value: v, comment: comment || null, ...scores } })),
    () => setValue(null),
  );
  const done = ["resolved", "closed"].includes(x.status);
  if (!done) return null;
  if (x.rating) {
    const given = SURVEY.map((k) => [k, x[`rating_${k}`]] as const).filter(([, v]) => v != null);
    return (
      <div className="rounded-xl border border-line bg-paper p-3 text-sm">
        {t("card.ratedAs")} {x.rating === "up" ? "👍" : "👎"} {x.rating_comment && <span className="text-muted">— {x.rating_comment}</span>}
        {given.length > 0 && (
          <div className="mt-1 text-muted">{given.map(([k, v]) => `${t(`card.survey.${k}`)}: ${v}/5`).join(" · ")}</div>
        )}
      </div>
    );
  }
  if (!d.is_requester) return null;
  return (
    <div className="rounded-xl border border-line bg-paper p-3 text-sm">
      <div className="mb-2 font-medium">{t("card.rateQuestion")}</div>
      <div className="flex gap-2">
        <Button variant={value === "up" ? "primary" : "secondary"} onClick={() => setValue("up")}>
          👍 {t("card.good")}
        </Button>
        <Button variant={value === "down" ? "primary" : "secondary"} onClick={() => setValue("down")}>
          👎 {t("card.bad")}
        </Button>
      </div>
      {value && (
        <div className="mt-2 space-y-2">
          <div className="text-muted">{t("card.survey.hint")}</div>
          {SURVEY.map((k) => (
            <ScoreRow key={k} label={t(`card.survey.${k}`)} value={scores[k]} onChange={(v) => setScores({ ...scores, [k]: v })} />
          ))}
          <textarea
            rows={2}
            value={comment}
            onChange={(e) => setComment(e.target.value)}
            placeholder={value === "down" ? t("card.badComment") : t("card.goodComment")}
            className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm"
          />
          <Button variant="primary" disabled={rate.isPending} onClick={() => rate.mutate(value)}>
            {t("card.sendRating")}
          </Button>
        </div>
      )}
      {rate.error && <ErrorBox error={rate.error} />}
    </div>
  );
}

/** Вставить готовый ответ с подстановками. */
export function ResponsePicker({ number, onInsert }: { number: number; onInsert: (text: string) => void }) {
  const { t } = useTranslation();
  const list = useQuery({ queryKey: ["response-templates"], queryFn: () => ok<ResponseTemplate[]>(api.GET("/api/helpdesk/response-templates")), staleTime: 60_000 });
  const render = useMutation({
    mutationFn: (id: string) => ok<{ body: string }>(api.GET("/api/helpdesk/tickets/{number}/response-templates/{id}", { params: { path: { number, id } } })),
    onSuccess: (r) => onInsert(r.body),
  });
  if (!list.data?.length) return null;
  return (
    <Select
      aria-label={t("card.responseTemplates")}
      className="w-auto"
      value=""
      onChange={(e) => {
        if (e.target.value) render.mutate(e.target.value);
      }}
    >
      <option value="">{t("card.responseTemplates")}</option>
      {list.data.map((r) => (
        <option key={r.id} value={r.id}>
          {r.title}
        </option>
      ))}
    </Select>
  );
}

/** Минуты работы: пусто — не учитывать. */
export function MinutesInput({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const { t } = useTranslation();
  return (
    <label className="inline-flex items-center gap-1 text-sm text-muted">
      {t("card.minutes")}
      <Input className="w-20" inputMode="numeric" value={value} onChange={(e) => onChange(e.target.value.replace(/\D/g, "").slice(0, 4))} />
    </label>
  );
}

export function CloseCodeSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const { t } = useTranslation();
  const codes = useCloseCodes();
  return (
    <Select aria-label={t("card.closeCode")} value={value} onChange={(e) => onChange(e.target.value)}>
      <option value="">{t("helpdesk.choose")}</option>
      {codes.data
        ?.filter((c) => c.code !== "duplicate")
        .map((c) => (
          <option key={c.code} value={c.code}>
            {c.label}
          </option>
        ))}
    </Select>
  );
}

