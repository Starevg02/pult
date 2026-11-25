// Создание заявки: обычная и «по звонку» за сотрудника.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { useMemo, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Category, type ComputerRow, type Created, type FoundObject, type Person, type ResourceView, type Site, type TicketContext, type TicketTemplate } from "../../api/client";
import { AssetPicker } from "./AssetPicker";
import { usePersonAssets } from "./cardUtil";
import { KbSuggest } from "../kb/KbPages";
import { AssistCategoryButton } from "./assist";
import { can, useMe } from "../../session";
import { Button, Card, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { fullName } from "../phonebookUtil";
import type { Uploaded } from "./helpdeskUtil";
import { Announcements, Editor } from "./parts";

type FormField = { key: string; label: string; type: string; required?: boolean; options?: string[] };

/** Путь в дереве: «Сеть / Wi-Fi». */
function paths(cats: Category[]) {
  const by = new Map(cats.map((c) => [c.id, c]));
  const name = (c: Category): string => {
    const p = c.parent_id ? by.get(c.parent_id) : undefined;
    return p ? `${name(p)} / ${c.name}` : c.name;
  };
  return cats.map((c) => ({ ...c, path: name(c) })).sort((a, b) => a.path.localeCompare(b.path));
}

function PersonPicker({ value, onPick }: { value: Person | null; onPick: (p: Person | null) => void }) {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const found = useQuery({
    queryKey: ["person-pick", q],
    queryFn: () => ok<Person[]>(api.GET("/api/phonebook/persons", { params: { query: { q, limit: 8 } } })),
    enabled: q.trim().length > 1,
  });
  if (value)
    return (
      <div className="flex items-center gap-2 rounded-lg border border-line px-3 py-2 text-sm">
        <span className="font-medium">{fullName(value)}</span>
        <span className="text-muted">{value.department_name}</span>
        <Button type="button" variant="ghost" className="ml-auto" onClick={() => onPick(null)}>
          {t("common.change")}
        </Button>
      </div>
    );
  return (
    <div>
      <Input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("helpdesk.searchPerson")} />
      {found.data && found.data.length > 0 && (
        <ul className="mt-1 divide-y divide-line rounded-lg border border-line">
          {found.data.map((p) => (
            <li key={p.id}>
              <button type="button" className="w-full px-3 py-2 text-left text-sm hover:bg-hover" onClick={() => onPick(p)}>
                {fullName(p)} <span className="text-muted">{p.department_name}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export { PersonPicker };

/** Поле «Компьютер»: инженеру — подсказки из AD, сотруднику — обычное поле. */
function ComputerInput({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const me = useMe().data;
  const [open, setOpen] = useState(false);
  const allowed = can(me, "helpdesk.ticket.work") || can(me, "helpdesk.ticket.create_on_behalf");
  const q = value.trim();
  const found = useQuery({
    queryKey: ["computer-pick", q],
    queryFn: () => ok<ComputerRow[]>(api.GET("/api/helpdesk/computers", { params: { query: { q, limit: 8 } } })),
    enabled: allowed && open && q.length > 1,
  });
  const list = found.data ?? [];
  return (
    <div className="relative">
      <Input
        className="font-mono"
        autoComplete="off"
        value={value}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
        }}
        onBlur={() => setOpen(false)}
      />
      {open && list.length > 0 && (
        <ul className="absolute z-10 mt-1 max-h-64 w-full divide-y divide-line overflow-auto rounded-lg border border-line bg-paper shadow-lg">
          {list.map((c) => (
            <li key={c.id}>
              <button
                type="button"
                className="w-full px-3 py-2 text-left text-sm hover:bg-hover"
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => {
                  onChange(c.name);
                  setOpen(false);
                }}
              >
                <span className="font-mono font-medium">{c.name}</span>
                {(c.description ?? c.operating_system) && <span className="ml-2 text-muted">{c.description ?? c.operating_system}</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Form({ cats, sites, ctx, onBehalf, initialCategory }: { cats: Category[]; sites: Site[]; ctx: TicketContext; onBehalf: boolean; initialCategory?: string }) {
  const { t } = useTranslation();
  const nav = useNavigate();
  const qc = useQueryClient();
  const tree = useMemo(() => paths(cats), [cats]);
  const [person, setPerson] = useState<Person | null>(null);
  const [channel, setChannel] = useState("phone");
  const [category, setCategory] = useState(() => (initialCategory && cats.some((c) => c.id === initialCategory) ? initialCategory : ""));
  const [site, setSite] = useState(ctx.site_id ?? "");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [files, setFiles] = useState<Uploaded[]>([]);
  // Кабинет и телефон не подставляются из справочника — их указывает человек. ПК — из активного сеанса.
  const [phone, setPhone] = useState("");
  const [room, setRoom] = useState("");
  const [computer, setComputer] = useState(ctx.computer_name ?? "");
  const [assets, setAssets] = useState<FoundObject[]>([]);
  const me = useMe().data;
  const engineer = can(me, "helpdesk.ticket.work");
  const requesterAssets = usePersonAssets(onBehalf ? person?.id : null);
  const own = onBehalf ? (requesterAssets.data ?? []) : ctx.assets;
  const [answers, setAnswers] = useState<Record<string, unknown>>({});
  // «Себе» ↔ «За сотрудника»: тема, описание и файлы остаются, заявитель, его оборудование и ПК — сбрасываются.
  const [wasBehalf, setWasBehalf] = useState(onBehalf);
  if (wasBehalf !== onBehalf) {
    setWasBehalf(onBehalf);
    setPerson(null);
    setAssets([]);
    setComputer(onBehalf ? "" : (ctx.computer_name ?? ""));
  }
  const [sameAs, setSameAs] = useState<number | null>(null);
  const [template, setTemplate] = useState("");
  const [done, setDone] = useState<Created | null>(null);
  const templates = useQuery({ queryKey: ["templates"], queryFn: () => ok<TicketTemplate[]>(api.GET("/api/helpdesk/templates")) });
  const applyTemplate = (id: string) => {
    setTemplate(id);
    const tp = templates.data?.find((x) => x.id === id);
    if (!tp) return;
    if (tp.title) setTitle(tp.title);
    if (tp.description) setDescription(tp.description);
    if (tp.category_id) setCategory(tp.category_id);
  };
  const cat = tree.find((c) => c.id === category);
  const schema = (cat?.form_schema ?? []) as unknown as FormField[];
  // «Запрос доступа»: один ресурс из каталога; согласует его владелец.
  const isAccess = cat?.code === "access";
  const [resource, setResource] = useState("");
  const resources = useQuery({
    queryKey: ["access-resources"],
    queryFn: () => ok<ResourceView[]>(api.GET("/api/helpdesk/access/resources")),
    enabled: isAccess,
  });
  const picked = resources.data?.find((r) => r.id === resource);

  const pickPerson = (p: Person | null) => {
    setPerson(p);
    setAssets([]);
    if (p) {
      setSite(p.site_id ?? "");
      setComputer("");
    }
  };

  const create = useMutation({
    mutationFn: () =>
      ok<Created>(
        api.POST("/api/helpdesk/tickets", {
          body: {
            title,
            description,
            category_id: category,
            site_id: site,
            contact_phone: phone || null,
            contact_room: room || null,
            computer_name: computer || null,
            form_answers: answers as never,
            files: files.map((f) => f.id),
            same_as: sameAs,
            on_behalf_of: onBehalf ? (person?.id ?? null) : null,
            channel: onBehalf ? channel : null,
            template_id: template || null,
            resource_id: isAccess ? resource || null : null,
            asset_ids: assets.map((a) => a.id),
          },
        }),
      ),
    onSuccess: (c) => {
      void qc.invalidateQueries({ queryKey: ["tickets"] });
      setDone(c);
    },
  });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    create.mutate();
  };
  if (done) {
    return (
      <Card className="mx-auto max-w-xl p-6">
        <h2 className="text-lg font-semibold">{t("helpdesk.created", { number: done.number })}</h2>
        <p className="mt-1 text-sm text-muted">{done.assignee_name ? t("helpdesk.assignedTo", { name: done.assignee_name }) : t("helpdesk.queued")}</p>
        {done.after_hours && (
          <p className="mt-3 rounded-lg bg-sunken px-3 py-2 text-sm">
            {t("helpdesk.afterHours", { at: done.after_hours.next_open })} {done.after_hours.emergency_phone && t("helpdesk.emergency", { phone: done.after_hours.emergency_phone })}
          </p>
        )}
        <div className="mt-4 flex gap-2">
          <Button variant="primary" onClick={() => void nav({ to: "/tickets/$number", params: { number: String(done.number) } })}>
            {t("helpdesk.openTicket")}
          </Button>
          <Link to="/" className="rounded-lg px-3 py-2 text-sm text-muted hover:bg-hover">
            {t("nav.home")}
          </Link>
        </div>
      </Card>
    );
  }
  return (
    <form onSubmit={submit} className="mx-auto max-w-2xl space-y-4">
      {!onBehalf && (
        <Announcements
          action={(a) =>
            a.ticket_number && (
              <label className="inline-flex items-center gap-2 text-sm">
                <input type="checkbox" checked={sameAs === a.ticket_number} onChange={(e) => setSameAs(e.target.checked ? (a.ticket_number ?? null) : null)} />
                {t("helpdesk.sameAsMe")}
              </label>
            )
          }
        />
      )}
      {ctx.after_hours && !onBehalf && (
        <div className="rounded-lg bg-sunken px-3 py-2 text-sm">
          {t("helpdesk.afterHours", { at: ctx.after_hours.next_open })} {ctx.after_hours.emergency_phone && t("helpdesk.emergency", { phone: ctx.after_hours.emergency_phone })}
        </div>
      )}
      <Card className="space-y-4 p-5">
        {onBehalf && (
          <div className="grid gap-4 md:grid-cols-[1fr_auto]">
            <Field group label={t("helpdesk.requester")}>
              <PersonPicker value={person} onPick={pickPerson} />
            </Field>
            <Field label={t("helpdesk.channelLabel")}>
              <Select value={channel} onChange={(e) => setChannel(e.target.value)}>
                <option value="phone">{t("helpdesk.channel.phone")}</option>
                <option value="walk_in">{t("helpdesk.channel.walk_in")}</option>
              </Select>
            </Field>
          </div>
        )}
        {!!templates.data?.length && (
          <Field label={t("card.template")} hint={t("card.templateHint")}>
            <Select value={template} onChange={(e) => applyTemplate(e.target.value)}>
              <option value="">—</option>
              {templates.data.map((tp) => (
                <option key={tp.id} value={tp.id}>
                  {tp.name}
                  {tp.checklist.length ? ` · ${t("svc.items", { n: tp.checklist.length })}` : ""}
                </option>
              ))}
            </Select>
          </Field>
        )}
        <div className="grid gap-4 md:grid-cols-2">
          <div>
            <Field label={t("helpdesk.category")}>
              <Select
                required
                value={category}
                onChange={(e) => {
                  setCategory(e.target.value);
                  // Поля формы — свои у каждой категории: ответы прежней не уходят на сервер.
                  setAnswers({});
                }}
              >
                <option value="">{t("helpdesk.choose")}</option>
                {tree.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.path}
                  </option>
                ))}
              </Select>
            </Field>
            <AssistCategoryButton
              title={title}
              description={description}
              onPick={(id) => {
                if (id !== category && tree.some((c) => c.id === id)) {
                  setCategory(id);
                  setAnswers({});
                }
              }}
            />
          </div>
          <Field label={t("helpdesk.site")}>
            <Select required value={site} onChange={(e) => setSite(e.target.value)}>
              <option value="">{t("helpdesk.choose")}</option>
              {sites.filter((s) => s.is_active).map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </Select>
          </Field>
        </div>
        {isAccess && (
          <Field label={t("access.resource")} hint={picked ? `${picked.description ? `${picked.description} · ` : ""}${t("access.approver")}: ${picked.owner_name ?? "—"}` : t("access.resourceHint")}>
            <Select
              required
              value={resource}
              onChange={(e) => {
                setResource(e.target.value);
                const r = resources.data?.find((x) => x.id === e.target.value);
                if (r && !title) setTitle(t("access.titleDefault", { resource: r.name }));
              }}
            >
              <option value="">{t("helpdesk.choose")}</option>
              {resources.data?.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </Select>
          </Field>
        )}
        <Field label={t("helpdesk.title")}>
          <Input required maxLength={300} value={title} onChange={(e) => setTitle(e.target.value)} />
        </Field>
        {!onBehalf && <KbSuggest text={title} />}
        <Field label={t("helpdesk.description")}>
          <Editor value={description} onChange={setDescription} files={files} onFiles={setFiles} rows={6} />
        </Field>
        {schema.map((f) => (
          <Field key={f.key} label={`${f.label}${f.required ? " *" : ""}`}>
            {f.type === "select" ? (
              <Select required={f.required} value={String(answers[f.key] ?? "")} onChange={(e) => setAnswers({ ...answers, [f.key]: e.target.value })}>
                <option value="">{t("helpdesk.choose")}</option>
                {f.options?.map((o) => (
                  <option key={o}>{o}</option>
                ))}
              </Select>
            ) : f.type === "bool" ? (
              <input type="checkbox" checked={answers[f.key] === true} onChange={(e) => setAnswers({ ...answers, [f.key]: e.target.checked })} />
            ) : (
              <Input required={f.required} value={String(answers[f.key] ?? "")} onChange={(e) => setAnswers({ ...answers, [f.key]: e.target.value })} />
            )}
          </Field>
        ))}
        <div className="grid gap-4 md:grid-cols-2">
          <Field label={t("helpdesk.phone")}>
            <Input value={phone} onChange={(e) => setPhone(e.target.value)} />
          </Field>
          <Field label={t("helpdesk.room")}>
            <Input value={room} onChange={(e) => setRoom(e.target.value)} />
          </Field>
          <Field label={t("helpdesk.computer")}>
            <ComputerInput value={computer} onChange={setComputer} />
          </Field>
        </div>
        <AssetPicker value={assets} onChange={setAssets} own={own} canSearch={engineer} ownLabel={onBehalf ? t("helpdesk.equipmentOf") : t("helpdesk.equipmentMine")} requesterKnown={!onBehalf || !!person} />
        {create.error && <ErrorBox error={create.error} />}
        <Button type="submit" variant="primary" className="py-2.5" disabled={create.isPending || (onBehalf && !person)}>
          {t("helpdesk.send")}
        </Button>
      </Card>
    </form>
  );
}

export function NewTicketPage() {
  const { t } = useTranslation();
  const me = useMe().data;
  const [mode, setMode] = useState<"self" | "behalf">("self");
  const { category } = useSearch({ from: "/tickets/new" });
  const cats = useQuery({ queryKey: ["categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories")) });
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const ctx = useQuery({ queryKey: ["ticket-context"], queryFn: () => ok<TicketContext>(api.GET("/api/helpdesk/context")) });
  const error = cats.error ?? sites.error ?? ctx.error;
  const behalf = can(me, "helpdesk.ticket.create_on_behalf");
  return (
    <div>
      <PageHeader title={t("helpdesk.newTitle")}>
        {behalf && (
          <Select aria-label={t("helpdesk.mode")} value={mode} onChange={(e) => setMode(e.target.value === "behalf" ? "behalf" : "self")}>
            <option value="self">{t("helpdesk.modeSelf")}</option>
            <option value="behalf">{t("helpdesk.modeBehalf")}</option>
          </Select>
        )}
      </PageHeader>
      {error ? <ErrorBox error={error} /> : cats.data && sites.data && ctx.data ? <Form cats={cats.data} sites={sites.data} ctx={ctx.data} onBehalf={mode === "behalf"} initialCategory={category} /> : <Spinner />}
    </div>
  );
}
