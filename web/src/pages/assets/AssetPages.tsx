// Каталог оборудования и карточка любого объекта с историей заявок (как в GLPI).
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Asset, type AssetEvent, type AssetType, type DiscoveredDevice, type AssetImportResult, type MyAsset, type ObjectCard, type Person, type Site } from "../../api/client";
import { formatDate } from "../../format";
import { can, useMe } from "../../session";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { fullName } from "../phonebookUtil";
import { dyn } from "../helpdesk/helpdeskUtil";
import { PersonPicker } from "../helpdesk/NewTicketPage";
import { StatusBadge, TicketLink } from "../helpdesk/parts";
import { AssetRelations } from "./CmdbParts";
import { AssetAlerts } from "./ServiceStatus";
import { ExternalFound } from "./NetboxParts";
import { statusesOf } from "./cmdbUtil";

type FormField = { key: string; label: string; type: string; options?: string[] };

function useTypes() {
  return useQuery({ queryKey: ["asset-types"], queryFn: () => ok<AssetType[]>(api.GET("/api/helpdesk/asset-types")) });
}

type Draft = {
  type_id: string;
  name: string;
  inventory_number: string;
  serial_number: string;
  status: string;
  site_id: string;
  room: string;
  notes: string;
  responsible: { id: string; name: string } | null;
  attributes: Record<string, unknown>;
};

const empty = (type_id = ""): Draft => ({ type_id, name: "", inventory_number: "", serial_number: "", status: "in_use", site_id: "", room: "", notes: "", responsible: null, attributes: {} });

function fromAsset(a: Asset): Draft {
  return {
    type_id: a.type_id,
    name: a.name,
    inventory_number: a.inventory_number ?? "",
    serial_number: a.serial_number ?? "",
    status: a.status,
    site_id: a.site_id ?? "",
    room: a.room ?? "",
    notes: a.notes ?? "",
    responsible: a.responsible_person_id ? { id: a.responsible_person_id, name: a.responsible_name ?? "" } : null,
    attributes: (a.attributes ?? {}) as Record<string, unknown>,
  };
}

/** Окно монтируется заново при каждом открытии: состояние формы берётся из пропсов. */
function AssetDialog({ onClose, asset }: { onClose: () => void; asset: Asset | null }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const types = useTypes();
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const [form, setF] = useState<Draft>(() => (asset ? fromAsset(asset) : empty()));
  const [pick, setPick] = useState(false);
  // Тип по умолчанию — первый из списка, когда он загрузится.
  const f = form.type_id ? form : { ...form, type_id: types.data?.[0]?.id ?? "" };
  const type = types.data?.find((x) => x.id === f.type_id);
  const fields = (type?.fields ?? []) as FormField[];
  const body = {
    type_id: f.type_id,
    name: f.name,
    inventory_number: f.inventory_number || null,
    serial_number: f.serial_number || null,
    status: f.status,
    site_id: f.site_id || null,
    room: f.room || null,
    notes: f.notes || null,
    responsible_person_id: f.responsible?.id ?? null,
    attributes: f.attributes as never,
  };
  const save = useMutation({
    mutationFn: () => (asset ? ok(api.PUT("/api/helpdesk/assets/{id}", { params: { path: { id: asset.id } }, body })) : ok(api.POST("/api/helpdesk/assets", { body }))),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["assets"] });
      void qc.invalidateQueries({ queryKey: ["object"] });
      // Счётчики в фильтре по типам.
      void qc.invalidateQueries({ queryKey: ["asset-types"] });
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={asset ? asset.name : t("assets.new")} wide>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t("assets.type")}>
          <Select value={f.type_id} onChange={(e) => setF({ ...f, type_id: e.target.value })}>
            {types.data?.map((x) => (
              <option key={x.id} value={x.id}>
                {x.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("assets.name")}>
          <Input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
        </Field>
        <Field label={t("assets.inventory")}>
          <Input value={f.inventory_number} onChange={(e) => setF({ ...f, inventory_number: e.target.value })} />
        </Field>
        <Field label={t("assets.serial")}>
          <Input value={f.serial_number} onChange={(e) => setF({ ...f, serial_number: e.target.value })} />
        </Field>
        <Field label={t("assets.status")}>
          <Select value={f.status} onChange={(e) => setF({ ...f, status: e.target.value })}>
            {statusesOf(type)
              .concat(statusesOf(type).includes(f.status) ? [] : [f.status])
              .map((s) => (
              <option key={s} value={s}>
                {dyn(t, `assets.statuses.${s}`)}
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
        <Field label={t("helpdesk.room")}>
          <Input value={f.room} onChange={(e) => setF({ ...f, room: e.target.value })} />
        </Field>
        <Field group label={t("assets.responsible")}>
          {pick ? (
            <PersonPicker
              value={null}
              onPick={(p: Person | null) => {
                setF({ ...f, responsible: p ? { id: p.id, name: fullName(p) } : null });
                setPick(false);
              }}
            />
          ) : (
            <div className="flex items-center gap-2 text-sm">
              <span className="flex-1">{f.responsible?.name ?? "—"}</span>
              <Button variant="ghost" onClick={() => setPick(true)}>
                {t("common.change")}
              </Button>
              {f.responsible && (
                <Button variant="ghost" onClick={() => setF({ ...f, responsible: null })}>
                  ✕
                </Button>
              )}
            </div>
          )}
        </Field>
        {fields.map((fl) => (
          <Field key={fl.key} label={fl.label}>
            {fl.type === "select" ? (
              <Select value={String(f.attributes[fl.key] ?? "")} onChange={(e) => setF({ ...f, attributes: { ...f.attributes, [fl.key]: e.target.value } })}>
                <option value="">—</option>
                {fl.options?.map((o) => (
                  <option key={o}>{o}</option>
                ))}
              </Select>
            ) : fl.type === "bool" ? (
              <input type="checkbox" checked={!!f.attributes[fl.key]} onChange={(e) => setF({ ...f, attributes: { ...f.attributes, [fl.key]: e.target.checked } })} />
            ) : (
              <Input value={String(f.attributes[fl.key] ?? "")} onChange={(e) => setF({ ...f, attributes: { ...f.attributes, [fl.key]: e.target.value } })} />
            )}
          </Field>
        ))}
        <div className="sm:col-span-2">
          <Field label={t("assets.notes")}>
            <textarea rows={3} value={f.notes} onChange={(e) => setF({ ...f, notes: e.target.value })} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
        </div>
      </div>
      {save.error && <ErrorBox error={save.error} />}
      <div className="mt-3 flex justify-end gap-2">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={!f.name.trim() || !f.type_id || save.isPending} onClick={() => save.mutate()}>
          {t("common.save")}
        </Button>
      </div>
    </Dialog>
  );
}

export function AssetsPage() {
  const { t } = useTranslation();
  const me = useMe().data;
  const manage = can(me, "helpdesk.assets.manage");
  const [q, setQ] = useState("");
  const [needle, setNeedle] = useState("");
  const [type, setType] = useState("");
  const [all, setAll] = useState(false);
  const [edit, setEdit] = useState<Asset | null | "new">(null);
  const [importing, setImporting] = useState(false);
  useEffect(() => {
    const h = setTimeout(() => setNeedle(q.trim()), 250);
    return () => clearTimeout(h);
  }, [q]);
  const types = useTypes();
  const list = useQuery({
    queryKey: ["assets", needle, type, all],
    queryFn: () => ok<Asset[]>(api.GET("/api/helpdesk/assets", { params: { query: { q: needle || undefined, type_id: type || undefined, all: all || undefined } } })),
  });
  return (
    <div>
      <PageHeader title={t("assets.title")}>
        <Link to="/licenses" className="inline-flex min-h-9 items-center rounded-lg px-3 text-sm text-accent hover:underline">
          {t("cmdb.licenses")}
        </Link>
        {manage && <Button onClick={() => setImporting(true)}>{t("assets.import.title")}</Button>}
        {manage && (
          <Button variant="primary" onClick={() => setEdit("new")}>
            {t("assets.new")}
          </Button>
        )}
      </PageHeader>
      {manage && <Discovered />}
      {manage && <ExternalFound />}
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <Input type="search" className="max-w-xs" aria-label={t("assets.search")} placeholder={t("assets.search")} value={q} onChange={(e) => setQ(e.target.value)} />
        <Select aria-label={t("assets.type")} className="w-auto" value={type} onChange={(e) => setType(e.target.value)}>
          <option value="">{t("assets.allTypes")}</option>
          {types.data?.map((x) => (
            <option key={x.id} value={x.id}>
              {x.name} ({x.count})
            </option>
          ))}
        </Select>
        <label className="inline-flex items-center gap-2 text-sm">
          <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} />
          {t("assets.showRetired")}
        </label>
      </div>
      {list.error ? (
        <ErrorBox error={list.error} />
      ) : list.isPending ? (
        <Spinner />
      ) : list.data.length === 0 ? (
        <Empty>{t("assets.empty")}</Empty>
      ) : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="text-left text-xs text-muted">
              <tr className="border-b border-line">
                <th className="px-3 py-2">{t("assets.name")}</th>
                <th className="px-3 py-2">{t("assets.type")}</th>
                <th className="px-3 py-2">{t("assets.inventory")}</th>
                <th className="hidden px-3 py-2 md:table-cell">{t("helpdesk.site")}</th>
                <th className="hidden px-3 py-2 lg:table-cell">{t("assets.responsible")}</th>
                <th className="px-3 py-2">{t("assets.status")}</th>
                <th className="px-3 py-2">{t("assets.openTickets")}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {list.data.map((a) => (
                <tr key={a.id} className="hover:bg-hover">
                  <td className="px-3 py-2">
                    <Link to="/objects/$kind/$id" params={{ kind: "asset", id: a.id }} className="font-medium text-accent hover:underline">
                      {a.name}
                    </Link>
                  </td>
                  <td className="px-3 py-2 text-muted">{a.type_name}</td>
                  <td className="px-3 py-2 font-mono text-xs">{a.inventory_number}</td>
                  <td className="hidden px-3 py-2 text-muted md:table-cell">
                    {a.site_name}
                    {a.room && ` · ${a.room}`}
                  </td>
                  <td className="hidden px-3 py-2 text-muted lg:table-cell">{a.responsible_name}</td>
                  <td className="px-3 py-2">
                    <Badge tone={a.status === "in_use" ? "ok" : a.status === "repair" ? "bad" : "muted"}>{dyn(t, `assets.statuses.${a.status}`)}</Badge>
                  </td>
                  <td className="px-3 py-2">{a.open_tickets > 0 ? <Badge tone="accent">{a.open_tickets}</Badge> : ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
      {edit !== null && <AssetDialog onClose={() => setEdit(null)} asset={edit === "new" ? null : edit} />}
      {importing && <ImportDialog onClose={() => setImporting(false)} />}
    </div>
  );
}

/** Карточка объекта: компьютер, сотрудник, учётка AD или оборудование — и все заявки с ним. */
export function ObjectPage() {
  const { t, i18n } = useTranslation();
  const { kind, id } = useParams({ from: "/objects/$kind/$id" });
  const me = useMe().data;
  const [edit, setEdit] = useState(false);
  const [hand, setHand] = useState<Handover | null>(null);
  const card = useQuery({ queryKey: ["object", kind, id], queryFn: () => ok<ObjectCard>(api.GET("/api/helpdesk/objects/{kind}/{id}", { params: { path: { kind, id } } })) });
  if (card.error) return <ErrorBox error={card.error} />;
  if (!card.data) return <Spinner />;
  const c = card.data;
  const a = c.asset;
  const attrs = Object.entries((a?.attributes ?? {}) as Record<string, unknown>).filter(([, v]) => v !== "" && v !== null && v !== false);
  return (
    <div className="mx-auto max-w-4xl">
      {kind === "asset" && (
        <Link to="/assets" className="mb-2 inline-block text-sm text-accent hover:underline">
          ← {t("assets.title")}
        </Link>
      )}
      <PageHeader title={c.label}>
        <Badge>{dyn(t, `card.kinds.${c.kind}`)}</Badge>
        {a && can(me, "helpdesk.assets.manage") && a.status !== "retired" && (
          <>
            {a.responsible_person_id ? (
              <>
                <Button onClick={() => setHand("transfer")}>{t("assets.handover.transfer")}</Button>
                <Button onClick={() => setHand("return")}>{t("assets.handover.return")}</Button>
              </>
            ) : (
              <Button onClick={() => setHand("issue")}>{t("assets.handover.issue")}</Button>
            )}
          </>
        )}
        {a && can(me, "helpdesk.assets.manage") && <Button onClick={() => setEdit(true)}>{t("common.edit")}</Button>}
      </PageHeader>
      {c.detail && <p className="mb-3 text-sm text-muted">{c.detail}</p>}
      {a && (
        <Card className="mb-4 p-4">
          <dl className="grid gap-x-6 gap-y-1 text-sm sm:grid-cols-2">
            {[
              [t("assets.type"), a.type_name],
              [t("assets.inventory"), a.inventory_number],
              [t("assets.serial"), a.serial_number],
              [t("assets.status"), dyn(t, `assets.statuses.${a.status}`)],
              [t("helpdesk.site"), a.site_name],
              [t("helpdesk.room"), a.room],
              [t("assets.responsible"), a.responsible_name],
              ...attrs.map(([k, v]) => [k, typeof v === "boolean" ? "✓" : String(v)]),
            ]
              .filter(([, v]) => v)
              .map(([k, v]) => (
                <div key={k} className="grid grid-cols-[9rem_1fr] gap-2">
                  <dt className="text-muted">{k}</dt>
                  <dd className="break-words">{v}</dd>
                </div>
              ))}
          </dl>
          {a.notes && <p className="mt-3 border-t border-line pt-2 text-sm whitespace-pre-wrap">{a.notes}</p>}
        </Card>
      )}
      {a && <AssetAlerts id={a.id} />}
      {a && <AssetRelations asset={a} />}
      <h2 className="mb-2 font-medium">{t("assets.history")}</h2>
      {c.tickets.length === 0 ? (
        <Empty>{t("assets.noTickets")}</Empty>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {c.tickets.map((x) => (
              <li key={x.number}>
                <TicketLink number={x.number} className="flex flex-wrap items-center gap-3 px-4 py-2 text-sm hover:bg-hover">
                  <span className="font-mono text-xs text-muted">#{x.number}</span>
                  <span className="min-w-0 flex-1 truncate">{x.title}</span>
                  <StatusBadge status={x.status} />
                  <span className="text-xs text-muted">{formatDate(x.created_at, i18n.language)}</span>
                </TicketLink>
              </li>
            ))}
          </ul>
        </Card>
      )}
      {a && c.events.length > 0 && <AssetHistory events={c.events} />}
      {a && edit && <AssetDialog onClose={() => setEdit(false)} asset={a} />}
      {a && hand && <HandoverDialog action={hand} asset={a} onClose={() => setHand(null)} />}
    </div>
  );
}

type Handover = "issue" | "return" | "transfer";

/** Выдать, вернуть или передать оборудование; запись попадает в историю. */
function HandoverDialog({ action, asset, onClose }: { action: Handover; asset: Asset; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [person, setPerson] = useState<Person | null>(null);
  const [status, setStatus] = useState("in_stock");
  const [comment, setComment] = useState("");
  const needPerson = action !== "return";
  const save = useMutation({
    mutationFn: () =>
      ok(
        api.POST("/api/helpdesk/assets/{id}/handover", {
          params: { path: { id: asset.id } },
          body: { action, person_id: needPerson ? (person?.id ?? null) : null, status: action === "return" ? status : null, comment: comment.trim() || null },
        }),
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["object"] });
      void qc.invalidateQueries({ queryKey: ["assets"] });
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={`${dyn(t, `assets.handover.${action}`)}: ${asset.name}`}>
      <div className="grid gap-3">
        {asset.responsible_name && action !== "issue" && (
          <p className="text-sm text-muted">
            {t("assets.handover.current")}: {asset.responsible_name}
          </p>
        )}
        {needPerson && (
          <Field group label={action === "issue" ? t("assets.handover.to") : t("assets.handover.newResponsible")}>
            {person ? (
              <div className="flex items-center gap-2 text-sm">
                <span className="flex-1">{fullName(person)}</span>
                <Button variant="ghost" onClick={() => setPerson(null)}>
                  {t("common.change")}
                </Button>
              </div>
            ) : (
              <PersonPicker value={null} onPick={setPerson} />
            )}
          </Field>
        )}
        {action === "return" && (
          <Field label={t("assets.handover.after")}>
            <Select value={status} onChange={(e) => setStatus(e.target.value)}>
              <option value="in_stock">{t("assets.statuses.in_stock")}</option>
              <option value="repair">{t("assets.statuses.repair")}</option>
            </Select>
          </Field>
        )}
        <Field label={t("assets.handover.comment")}>
          <textarea rows={2} maxLength={1000} value={comment} onChange={(e) => setComment(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
        </Field>
      </div>
      {save.error && <ErrorBox error={save.error} />}
      <div className="mt-3 flex justify-end gap-2">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={(needPerson && !person) || save.isPending} onClick={() => save.mutate()}>
          {dyn(t, `assets.handover.${action}`)}
        </Button>
      </div>
    </Dialog>
  );
}

/** Значение поля из записи «изменено»: состояние — словами, пустое — прочерком. */
function shown(t: ReturnType<typeof useTranslation>["t"], field: string, v: unknown): string {
  if (v === null || v === undefined || v === "") return "—";
  if (field === "status") return dyn(t, `assets.statuses.${String(v)}`);
  if (typeof v === "object")
    return (
      Object.entries(v as Record<string, unknown>)
        .map(([k, x]) => `${k}: ${String(x)}`)
        .join(", ") || "—"
    );
  return String(v);
}

/** История оборудования: выдача, возврат, передача, правки карточки. */
function AssetHistory({ events }: { events: AssetEvent[] }) {
  const { t, i18n } = useTranslation();
  return (
    <>
      <h2 className="mt-4 mb-2 font-medium">{t("assets.events.title")}</h2>
      <Card>
        <ul className="divide-y divide-line">
          {events.map((e, i) => {
            const changes = Object.entries((e.changes ?? {}) as Record<string, [unknown, unknown]>);
            return (
              <li key={i} className="px-4 py-2 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <Badge tone={e.kind === "issued" || e.kind === "transferred" ? "accent" : e.kind === "returned" ? "ok" : "muted"}>{dyn(t, `assets.events.${e.kind}`)}</Badge>
                  {(e.from_name || e.to_name) && (
                    <span>
                      {e.from_name ?? "—"} → {e.to_name ?? "—"}
                    </span>
                  )}
                  <span className="ml-auto text-xs text-muted">
                    {formatDate(e.created_at, i18n.language)}
                    {e.actor_name && ` · ${e.actor_name}`}
                  </span>
                </div>
                {changes.length > 0 && (
                  <ul className="mt-1 text-xs text-muted">
                    {changes.map(([k, [from, to]]) => (
                      <li key={k}>
                        {dyn(t, `assets.fields.${k}`)}: {k === "type_id" || k === "site_id" ? t("assets.events.changed") : `${shown(t, k, from)} → ${shown(t, k, to)}`}
                      </li>
                    ))}
                  </ul>
                )}
                {e.comment && <p className="mt-1 whitespace-pre-wrap">{e.comment}</p>}
              </li>
            );
          })}
        </ul>
      </Card>
    </>
  );
}

/** «Моё оборудование»: что закреплено за сотрудником, только просмотр. */
export function MyAssetsPage() {
  const { t, i18n } = useTranslation();
  const list = useQuery({ queryKey: ["my-assets"], queryFn: () => ok<MyAsset[]>(api.GET("/api/helpdesk/my/assets")) });
  return (
    <div className="mx-auto max-w-4xl">
      <PageHeader title={t("assets.mine.title")} />
      <p className="mb-3 text-sm text-muted">{t("assets.mine.hint")}</p>
      {list.error ? (
        <ErrorBox error={list.error} />
      ) : list.isPending ? (
        <Spinner />
      ) : list.data.length === 0 ? (
        <Empty>{t("assets.mine.empty")}</Empty>
      ) : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="text-left text-xs text-muted">
              <tr className="border-b border-line">
                <th className="px-3 py-2">{t("assets.name")}</th>
                <th className="px-3 py-2">{t("assets.type")}</th>
                <th className="px-3 py-2">{t("assets.inventory")}</th>
                <th className="hidden px-3 py-2 md:table-cell">{t("helpdesk.site")}</th>
                <th className="px-3 py-2">{t("assets.status")}</th>
                <th className="hidden px-3 py-2 sm:table-cell">{t("assets.mine.since")}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {list.data.map((a) => (
                <tr key={a.id}>
                  <td className="px-3 py-2 font-medium">
                    {a.name}
                    {a.serial_number && <div className="font-mono text-xs font-normal text-muted">{a.serial_number}</div>}
                  </td>
                  <td className="px-3 py-2 text-muted">{a.type_name}</td>
                  <td className="px-3 py-2 font-mono text-xs">{a.inventory_number}</td>
                  <td className="hidden px-3 py-2 text-muted md:table-cell">
                    {a.site_name}
                    {a.room && ` · ${a.room}`}
                  </td>
                  <td className="px-3 py-2">
                    <Badge tone={a.status === "in_use" ? "ok" : a.status === "repair" ? "bad" : "muted"}>{dyn(t, `assets.statuses.${a.status}`)}</Badge>
                  </td>
                  <td className="hidden px-3 py-2 text-xs text-muted sm:table-cell">{a.since ? formatDate(a.since, i18n.language) : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
      <p className="mt-3 text-sm">
        <Link to="/tickets/new" className="text-accent hover:underline">
          {t("assets.mine.report")}
        </Link>
      </p>
    </div>
  );
}

/** Не больше этого размера файла CSV (сервер принимает до 5000 строк). */
const MAX_CSV = 2_000_000;

/** Импорт из CSV: сначала проверка без записи, потом импорт. Известные инвентарные номера пропускаются. */
function ImportDialog({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [csv, setCsv] = useState("");
  const [tooBig, setTooBig] = useState(false);
  const run = useMutation({
    mutationFn: (dry_run: boolean) => ok<AssetImportResult>(api.POST("/api/helpdesk/assets/import", { body: { csv, dry_run } })),
    onSuccess: (r) => {
      if (!r.dry_run) {
        void qc.invalidateQueries({ queryKey: ["assets"] });
        void qc.invalidateQueries({ queryKey: ["asset-types"] });
      }
    },
  });
  const r = run.data;
  const onFile = async (f: File | undefined) => {
    run.reset();
    const big = !!f && f.size > MAX_CSV;
    setTooBig(big);
    setCsv(f && !big ? await f.text() : "");
  };
  return (
    <Dialog open onClose={onClose} title={t("assets.import.title")} wide>
      <p className="mb-2 text-sm text-muted">{t("assets.import.hint")}</p>
      <pre className="mb-3 overflow-x-auto rounded-lg bg-hover px-3 py-2 font-mono text-xs">{t("assets.import.sample")}</pre>
      <input type="file" accept=".csv,text/csv" className="text-sm" aria-label={t("assets.import.file")} onChange={(e) => void onFile(e.target.files?.[0])} />
      {tooBig && <p className="mt-2 text-sm text-bad">{t("assets.import.tooBig")}</p>}
      {run.error && <ErrorBox error={run.error} />}
      {r && (
        <div className="mt-3 text-sm" role="status">
          <p className="font-medium">{t(r.dry_run ? "assets.import.checked" : "assets.import.done", { created: r.created, skipped: r.skipped, errors: r.errors.length })}</p>
          {r.errors.length > 0 && (
            <ul className="mt-1 max-h-48 overflow-y-auto text-xs text-bad">
              {r.errors.map((e) => (
                <li key={e.row}>
                  {t("assets.import.row", { row: e.row })}: {e.message}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      <div className="mt-3 flex justify-end gap-2">
        <Button onClick={onClose}>{r && !r.dry_run ? t("common.close") : t("common.cancel")}</Button>
        <Button disabled={!csv || run.isPending} onClick={() => run.mutate(true)}>
          {t("assets.import.check")}
        </Button>
        <Button variant="primary" disabled={!csv || run.isPending || !r?.dry_run || r.created === 0} onClick={() => run.mutate(false)}>
          {t("assets.import.run")}
        </Button>
      </div>
    </Dialog>
  );
}

/** Принять ПК в реестр: тип (по умолчанию — тип с полем «ОС»), название, инв. номер, площадка, ответственный. */
function AcceptDialog({ device, onClose }: { device: DiscoveredDevice; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const types = useTypes();
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const [typeId, setTypeId] = useState("");
  const [name, setName] = useState(device.hostname);
  const [inventory, setInventory] = useState("");
  const [site, setSite] = useState("");
  const [person, setPerson] = useState<Person | null>(null);
  const guess = types.data?.find((x) => ((x.fields ?? []) as FormField[]).some((f) => f.key === "os")) ?? types.data?.[0];
  const type = typeId || guess?.id || "";
  const save = useMutation({
    mutationFn: () =>
      ok(
        api.POST("/api/helpdesk/assets/discovered/{id}/accept", {
          params: { path: { id: device.device_id } },
          body: { type_id: type, name: name.trim() || null, inventory_number: inventory.trim() || null, site_id: site || null, responsible_person_id: person?.id ?? null },
        }),
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["discovered"] });
      void qc.invalidateQueries({ queryKey: ["assets"] });
      void qc.invalidateQueries({ queryKey: ["asset-types"] });
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={`${t("assets.found.accept")}: ${device.hostname}`}>
      <div className="grid gap-3">
        <Field label={t("assets.type")}>
          <Select value={type} onChange={(e) => setTypeId(e.target.value)}>
            {types.data?.map((x) => (
              <option key={x.id} value={x.id}>
                {x.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("assets.name")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label={t("assets.inventory")}>
          <Input value={inventory} onChange={(e) => setInventory(e.target.value)} />
        </Field>
        <Field label={t("helpdesk.site")}>
          <Select value={site} onChange={(e) => setSite(e.target.value)}>
            <option value="">—</option>
            {sites.data?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
        <Field group label={t("assets.responsible")}>
          {person ? (
            <div className="flex items-center gap-2 text-sm">
              <span className="flex-1">{fullName(person)}</span>
              <Button variant="ghost" onClick={() => setPerson(null)}>
                {t("common.change")}
              </Button>
            </div>
          ) : (
            <PersonPicker value={null} onPick={setPerson} />
          )}
        </Field>
      </div>
      {save.error && <ErrorBox error={save.error} />}
      <div className="mt-3 flex justify-end gap-2">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={!type || save.isPending} onClick={() => save.mutate()}>
          {t("assets.found.accept")}
        </Button>
      </div>
    </Dialog>
  );
}

/** ПК, о которых сообщил агент удалённой поддержки, но которых нет в реестре: принять или отклонить. */
function Discovered() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [rejected, setRejected] = useState(false);
  const [accept, setAccept] = useState<DiscoveredDevice | null>(null);
  const fetchList = (r: boolean) => ok<DiscoveredDevice[]>(api.GET("/api/helpdesk/assets/discovered", { params: { query: { rejected: r || undefined } } }));
  const pending = useQuery({ queryKey: ["discovered", false], queryFn: () => fetchList(false) });
  const declined = useQuery({ queryKey: ["discovered", true], queryFn: () => fetchList(true), enabled: open && rejected });
  const act = useMutation({
    mutationFn: ({ id, restore }: { id: string; restore: boolean }) =>
      ok(restore ? api.POST("/api/helpdesk/assets/discovered/{id}/restore", { params: { path: { id } } }) : api.POST("/api/helpdesk/assets/discovered/{id}/reject", { params: { path: { id } } })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["discovered"] }),
  });
  if (!pending.data || (pending.data.length === 0 && !open)) return null;
  const list = rejected ? declined : pending;
  return (
    <Card className="mb-3">
      <button type="button" className="flex w-full items-center justify-between gap-2 px-4 py-2 text-left text-sm" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span>{t("assets.found.summary", { count: pending.data.length })}</span>
        <span className="text-accent">{open ? t("assets.found.hide") : t("assets.found.review")}</span>
      </button>
      {open && (
        <div className="border-t border-line">
          {act.error && <ErrorBox error={act.error} />}
          {list.error ? (
            <ErrorBox error={list.error} />
          ) : !list.data ? (
            <Spinner />
          ) : list.data.length === 0 ? (
            <Empty>{t("assets.found.empty")}</Empty>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead className="text-left text-xs text-muted">
                  <tr className="border-b border-line">
                    <th className="px-3 py-2">{t("assets.found.host")}</th>
                    <th className="hidden px-3 py-2 md:table-cell">{t("assets.found.os")}</th>
                    <th className="hidden px-3 py-2 sm:table-cell">IP</th>
                    <th className="hidden px-3 py-2 lg:table-cell">{t("assets.found.seen")}</th>
                    <th className="px-3 py-2" />
                  </tr>
                </thead>
                <tbody className="divide-y divide-line">
                  {list.data.map((d) => (
                    <tr key={d.device_id}>
                      <td className="px-3 py-2 font-medium">
                        {d.hostname}
                        {d.fqdn && <div className="font-mono text-xs font-normal text-muted">{d.fqdn}</div>}
                        {d.model && (
                          <div className="text-xs font-normal text-muted">
                            {d.model}
                            {d.serial_number && <span className="font-mono"> · {d.serial_number}</span>}
                          </div>
                        )}
                      </td>
                      <td className="hidden px-3 py-2 text-muted md:table-cell">{[d.os, d.os_version].filter(Boolean).join(" ")}</td>
                      <td className="hidden px-3 py-2 font-mono text-xs sm:table-cell">{d.ip_addrs.join(", ")}</td>
                      <td className="hidden px-3 py-2 text-xs text-muted lg:table-cell">{d.last_seen_at ? formatDate(d.last_seen_at, i18n.language) : "—"}</td>
                      <td className="px-3 py-2 text-right whitespace-nowrap">
                        {rejected ? (
                          <Button variant="ghost" disabled={act.isPending} onClick={() => act.mutate({ id: d.device_id, restore: true })}>
                            {t("common.restore")}
                          </Button>
                        ) : (
                          <>
                            <Button variant="ghost" onClick={() => setAccept(d)}>
                              {t("assets.found.accept")}
                            </Button>
                            <Button variant="ghost" disabled={act.isPending} onClick={() => act.mutate({ id: d.device_id, restore: false })}>
                              {t("assets.found.reject")}
                            </Button>
                          </>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <label className="flex items-center gap-2 border-t border-line px-4 py-2 text-sm">
            <input type="checkbox" checked={rejected} onChange={(e) => setRejected(e.target.checked)} />
            {t("assets.found.showRejected")}
          </label>
        </div>
      )}
      {accept && <AcceptDialog device={accept} onClose={() => setAccept(null)} />}
    </Card>
  );
}
