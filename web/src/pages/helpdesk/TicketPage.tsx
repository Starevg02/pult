// Карточка заявки: лента, ответ, действия, свойства.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "@tanstack/react-router";
import { AccessPanel, AccountPanel, Operations } from "./directory";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Category, type EngineerView, type FeedItem, type Person, type Site, type TicketDetail, type TicketHead } from "../../api/client";
import { formatDate } from "../../format";
import { ignoreHotkey } from "../../hotkeys";
import { can, useMe, useModule } from "../../session";
import { Badge, Button, Card, Dialog, ErrorBox, Field, Input, MenuButton, Select, Spinner, type MenuEntry } from "../../ui";
import {
  IconBan,
  IconCheck,
  IconCheckCircle,
  IconFileText,
  IconHelp,
  IconLink,
  IconMegaphone,
  IconMerge,
  IconMore,
  IconPause,
  IconRotate,
  IconShield,
  IconTransfer,
  IconUserCheck,
} from "../../icons";
import { dyn, PRIORITIES, type Uploaded } from "./helpdeskUtil";
import { PersonPicker } from "./NewTicketPage";
import { AssistDraftButton, AssistSummary } from "./assist";
import { AttachmentList, Editor, PriorityBadge, RichText, StatusBadge, TicketLink } from "./parts";
import {
  AssigneeControl,
  Checklist,
  CloseCodeSelect,
  GroupControl,
  ImpactUrgency,
  Links,
  MinutesInput,
  Objects,
  Rating,
  ResponsePicker,
  SlaBadge,
  SlaPanel,
  Watchers,
} from "./card";
import { closeCodeLabel, minutesOf, useCloseCodes } from "./cardUtil";
import { TicketRemote } from "../remote/RemotePages";
import { ItilPanel, NewItilDialog } from "./ItilPages";

const OPEN = ["new", "assigned", "in_progress", "waiting", "waiting_approval", "on_hold"];

function useAction(number: number) {
  const qc = useQueryClient();
  return <T,>(fn: (v: T) => Promise<unknown>, after?: () => void, toast?: string) =>
    // eslint-disable-next-line react-hooks/rules-of-hooks -- фабрика вызывается на верхнем уровне компонента
    useMutation({
      mutationFn: fn,
      // Итог действия — всплывающим сообщением (MutationCache в main.tsx).
      meta: { toast },
      onSuccess: () => {
        void qc.invalidateQueries({ queryKey: ["ticket", number] });
        void qc.invalidateQueries({ queryKey: ["tickets"] });
        after?.();
      },
    });
}

function FeedEntry({ item, lang }: { item: FeedItem; lang: string }) {
  const { t } = useTranslation();
  if (item.type === "event") {
    const to = typeof item.to === "string" ? item.to : "";
    const base =
      item.kind === "status"
        ? t("helpdesk.feed.status", { status: dyn(t, `helpdesk.status.${to}`) })
        : item.kind === "change_submitted"
          ? t("helpdesk.feed.change_submitted", { state: dyn(t, `itil.approval.${to}`) })
          : item.kind === "problem_linked" || item.kind === "problem_unlinked"
            ? t(`helpdesk.feed.${item.kind}`, { number: String(item.to ?? item.from ?? "") })
            : t(`helpdesk.feed.${item.kind}`, { defaultValue: item.kind ?? "" });
    // У события правила в from — его название.
    const text = item.kind === "rule" && typeof item.from === "string" ? `${base} «${item.from}»` : base;
    return (
      <li className="px-1 text-xs text-muted">
        {formatDate(item.at, lang)} · {item.author ?? t("helpdesk.system")} — {text}
      </li>
    );
  }
  return (
    <li className={`rounded-xl border p-3 ${item.internal ? "border-yellow-500/40 bg-yellow-500/10" : "border-line bg-paper"}`}>
      <div className="mb-1 flex flex-wrap items-center gap-2 text-xs text-muted">
        <span className="font-medium text-ink">{item.author ?? t("helpdesk.system")}</span>
        {item.internal && <Badge tone="accent">{t("helpdesk.internalNote")}</Badge>}
        {item.source === "email" && <Badge>{t("helpdesk.viaEmail")}</Badge>}
        <span className="ml-auto">{formatDate(item.at, lang)}</span>
      </div>
      <RichText text={item.body ?? ""} />
      <AttachmentList items={item.attachments} text={item.body ?? ""} />
    </li>
  );
}

function Prop({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[7rem_1fr] gap-2 py-1 text-sm">
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

function Reply({ d, focus, tab, setTab }: { d: TicketDetail; focus: number; tab: "public" | "internal"; setTab: (t: "public" | "internal") => void }) {
  const { t } = useTranslation();
  const n = d.ticket.number;
  const action = useAction(n);
  const [body, setBody] = useState("");
  const [files, setFiles] = useState<Uploaded[]>([]);
  const [broadcast, setBroadcast] = useState(false);
  const [minutes, setMinutes] = useState("");
  const [uploading, setUploading] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const clear = () => {
    setBody("");
    setFiles([]);
    setBroadcast(false);
    setMinutes("");
  };
  const send = action(
    () =>
      ok(
        api.POST("/api/helpdesk/tickets/{number}/comments", {
          params: { path: { number: n } },
          body: { body, internal: tab === "internal", files: files.map((f) => f.id), broadcast, minutes: d.can_work ? minutesOf(minutes) : null },
        }),
      ),
    clear,
    tab === "internal" ? t("helpdesk.done.noteSaved") : t("helpdesk.done.sent"),
  );
  useEffect(() => {
    if (focus) area.current?.focus();
  }, [focus]);
  const closed = d.ticket.status === "closed" || d.ticket.status === "canceled";
  if (closed && d.can_work) return null;
  return (
    <Card className="p-3">
      {d.can_work && (
        <div className="mb-2 flex gap-1">
          <Button variant={tab === "public" ? "primary" : "ghost"} onClick={() => setTab("public")}>
            {t("helpdesk.replyTab")}
          </Button>
          <Button variant={tab === "internal" ? "primary" : "ghost"} onClick={() => setTab("internal")}>
            {t("helpdesk.noteTab")}
          </Button>
        </div>
      )}
      <Editor textRef={area} value={body} onChange={setBody} files={files} onFiles={setFiles} onBusy={setUploading} tone={tab === "internal" ? "internal" : undefined} rows={4} placeholder={closed ? t("helpdesk.closedReplyHint") : undefined} />
      {send.error && <ErrorBox error={send.error} />}
      <div className="mt-2 flex flex-wrap items-center gap-3">
        <Button variant="primary" disabled={send.isPending || uploading || (!body.trim() && files.length === 0)} onClick={() => send.mutate(undefined)}>
          {tab === "internal" ? t("helpdesk.saveNote") : t("helpdesk.send")}
        </Button>
        {d.can_work && <MinutesInput value={minutes} onChange={setMinutes} />}
        {d.can_work && <ResponsePicker number={n} onInsert={(text) => setBody((b) => (b.trim() ? `${b}\n\n${text}` : text))} />}
        {d.can_work && tab === "public" && <AssistDraftButton number={n} onInsert={(text) => setBody((b) => (b.trim() ? `${b}\n\n${text}` : text))} />}
        {d.ticket.is_major && tab === "public" && d.can_work && (
          <label className="inline-flex items-center gap-2 text-sm">
            <input type="checkbox" checked={broadcast} onChange={(e) => setBroadcast(e.target.checked)} />
            {t("helpdesk.broadcast")}
          </label>
        )}
      </div>
    </Card>
  );
}

type DialogKind = "resolve" | "transfer" | "waiting" | "hold" | "merge" | "major" | "link" | "verify" | "notResolved" | null;

function Actions({ d, dialog, setDialog }: { d: TicketDetail; dialog: DialogKind; setDialog: (k: DialogKind) => void }) {
  const { t } = useTranslation();
  const me = useMe().data;
  const x = d.ticket;
  const n = x.number;
  // Закрыть диалог: поля и ошибки у диалогов общие — причина из одного не должна уйти в другой.
  const close = () => {
    setDialog(null);
    setText("");
    setTitle("");
    setFiles([]);
    setTarget("");
    setParent("");
    setPerson(null);
    setCode("");
    setMinutes("");
    for (const m of [resolve, waiting, hold, merge, transfer, major, link, verify, notResolved]) m.reset();
  };
  const action = useAction(n);
  const path = { params: { path: { number: n } } };
  const take = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/take", path)), undefined, t("helpdesk.done.taken"));
  const cancel = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/cancel", { ...path, body: {} })), undefined, t("helpdesk.done.canceled"));
  const confirm = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/confirm", path)), undefined, t("helpdesk.done.confirmed"));
  const reopen = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/reopen", path)), undefined, t("helpdesk.done.reopened"));
  const [text, setText] = useState("");
  const [title, setTitle] = useState("");
  const [files, setFiles] = useState<Uploaded[]>([]);
  const [target, setTarget] = useState("");
  const [line, setLine] = useState("l1");
  const [parent, setParent] = useState("");
  const [person, setPerson] = useState<Person | null>(null);
  const [code, setCode] = useState("");
  const [minutes, setMinutes] = useState("");
  const nav = useNavigate();
  const [uploading, setUploading] = useState(false);
  const done = () => close();
  const resolve = action(
    () => ok(api.POST("/api/helpdesk/tickets/{number}/resolve", { ...path, body: { comment: text, files: files.map((f) => f.id), closure_code: code, minutes: minutesOf(minutes) } })),
    done,
    t("helpdesk.done.resolved"),
  );
  const hold = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/hold", { ...path, body: { comment: text, minutes: minutesOf(minutes) } })), done, t("helpdesk.done.hold"));
  const merge = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/merge", { ...path, body: { into: Number(parent) } })), () => {
    const into = Number(parent);
    done();
    void nav({ to: "/tickets/$number", params: { number: String(into) } });
  }, t("helpdesk.done.merged"));
  const waiting = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/waiting", { ...path, body: { comment: text || null } })), done, t("helpdesk.done.waiting"));
  const transfer = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/transfer", { ...path, body: { assignee_id: target, line, comment: text } })), done, t("helpdesk.done.transferred"));
  const major = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/major", { ...path, body: { title, body: text, site_ids: [] } })), done, t("helpdesk.done.major"));
  const link = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/link", { ...path, body: { parent: parent ? Number(parent) : null } })), done, t("helpdesk.done.linked"));
  const verify = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/verify-identity", { ...path, body: { person_id: person?.id ?? "" } })), done, t("helpdesk.done.verified"));
  const notResolved = action(() => ok(api.POST("/api/helpdesk/tickets/{number}/not-resolved", { ...path, body: { comment: text } })), done, t("helpdesk.done.returned"));
  const engineers = useQuery({
    queryKey: ["engineers", x.site_id],
    queryFn: () => ok<EngineerView[]>(api.GET("/api/helpdesk/engineers", { params: { query: { site_id: x.site_id ?? undefined } } })),
    enabled: dialog === "transfer",
  });
  const open = OPEN.includes(x.status);
  const error = [take, cancel, confirm, reopen].map((m) => m.error).find(Boolean);
  const dialogError = [resolve, waiting, hold, merge, transfer, major, link, verify, notResolved].map((m) => m.error).find(Boolean);
  const canCancel = open && (d.can_work || (d.is_requester && !x.first_response_at));
  const askCancel = () => {
    if (window.confirm(t("helpdesk.cancelConfirm"))) cancel.mutate(undefined);
  };
  // Редкие действия инженера — в меню «Ещё» (кнопок на экране — две-три).
  const more: MenuEntry[] = !d.can_work
    ? []
    : [
        ...(open && x.status !== "waiting" ? [{ key: "waiting", label: t("helpdesk.toWaiting"), icon: <IconHelp />, onSelect: () => setDialog("waiting") }] : []),
        ...(open && x.status !== "on_hold" ? [{ key: "hold", label: t("card.hold"), icon: <IconPause />, onSelect: () => setDialog("hold") }] : []),
        ...(open && !x.is_major ? [{ key: "merge", label: t("card.merge"), icon: <IconMerge />, onSelect: () => setDialog("merge") }] : []),
        ...(open && !x.is_major ? [{ key: "link", label: t("helpdesk.link"), icon: <IconLink />, onSelect: () => setDialog("link") }] : []),
        ...(open && !x.is_major && !x.parent_number ? [{ key: "major", label: t("helpdesk.makeMajor"), icon: <IconMegaphone />, onSelect: () => setDialog("major") }] : []),
        ...(can(me, "helpdesk.kb.edit") ? [{ key: "kb", label: t("card.toKb"), icon: <IconFileText />, onSelect: () => void nav({ to: "/kb/new", search: { ticket: n } }) }] : []),
        ...(x.unverified ? [{ key: "verify", label: t("helpdesk.verifyIdentity"), icon: <IconShield />, onSelect: () => setDialog("verify") }] : []),
        ...(canCancel ? [{ key: "cancel", label: t("helpdesk.cancel"), icon: <IconBan />, danger: true, separated: true, disabled: cancel.isPending, onSelect: askCancel }] : []),
      ];
  const box = (run: () => void, disabled: boolean, label: string) => (
    <>
      {dialogError && <ErrorBox error={dialogError} />}
      <div className="flex justify-end gap-2 pt-1">
        <Button onClick={close}>{t("common.cancel")}</Button>
        <Button variant="primary" disabled={disabled} onClick={run}>
          {label}
        </Button>
      </div>
    </>
  );
  return (
    <div className="flex flex-wrap items-center gap-2">
      {d.can_work && open && x.assignee_id !== me?.user_id && (
        <Button variant="primary" disabled={take.isPending} onClick={() => take.mutate(undefined)}>
          <IconUserCheck />
          {t("helpdesk.take")}
        </Button>
      )}
      {d.can_work && open && x.assignee_id === me?.user_id && x.status === "assigned" && (
        <Button variant="primary" disabled={take.isPending} onClick={() => take.mutate(undefined)}>
          <IconUserCheck />
          {t("helpdesk.start")}
        </Button>
      )}
      {d.can_work && open && (
        <Button onClick={() => setDialog("resolve")}>
          <IconCheckCircle className="text-ok" />
          {t("helpdesk.resolve")}
        </Button>
      )}
      {d.can_work && open && (
        <Button onClick={() => setDialog("transfer")}>
          <IconTransfer className="text-muted" />
          {t("helpdesk.transfer")}
        </Button>
      )}
      {more.length > 0 && <MenuButton label={t("common.more")} icon={<IconMore />} items={more} />}
      {/* Заявителю отмена — отдельной кнопкой: других действий у него обычно нет. */}
      {!d.can_work && canCancel && (
        <Button variant="ghost" disabled={cancel.isPending} onClick={askCancel}>
          <IconBan />
          {t("helpdesk.cancel")}
        </Button>
      )}
      {d.is_requester && x.status === "resolved" && (
        <>
          <Button variant="primary" disabled={confirm.isPending} onClick={() => confirm.mutate(undefined)}>
            <IconCheck />
            {t("helpdesk.yesResolved")}
          </Button>
          <Button onClick={() => setDialog("notResolved")}>
            <IconRotate className="text-muted" />
            {t("helpdesk.notResolved")}
          </Button>
        </>
      )}
      {x.status === "closed" && me?.is_superadmin && (
        <Button disabled={reopen.isPending} onClick={() => reopen.mutate(undefined)}>
          <IconRotate className="text-muted" />
          {t("helpdesk.reopen")}
        </Button>
      )}
      {error && <ErrorBox error={error} />}

      <Dialog open={dialog === "resolve"} onClose={close} title={t("helpdesk.resolve")}>
        <div className="space-y-3">
          <Field label={t("helpdesk.resolveComment")}>
            <Editor value={text} onChange={setText} files={files} onFiles={setFiles} onBusy={setUploading} rows={4} />
          </Field>
          <div className="grid gap-3 sm:grid-cols-[1fr_auto]">
            <Field label={t("card.closeCode")}>
              <CloseCodeSelect value={code} onChange={setCode} />
            </Field>
            <div className="self-end pb-1">
              <MinutesInput value={minutes} onChange={setMinutes} />
            </div>
          </div>
          {d.checklist.some((i) => i.is_required && !i.done_at) && <p className="text-sm text-bad">{t("card.checklistOpen")}</p>}
          {x.is_major && <p className="text-sm text-muted">{t("helpdesk.majorResolveHint", { n: d.children.length })}</p>}
          {box(() => resolve.mutate(undefined), resolve.isPending || uploading || !text.trim() || !code, t("helpdesk.resolve"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "waiting"} onClose={close} title={t("helpdesk.toWaiting")}>
        <div className="space-y-3">
          <Field label={t("helpdesk.waitingQuestion")}>
            <textarea rows={3} value={text} onChange={(e) => setText(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
          {box(() => waiting.mutate(undefined), waiting.isPending, t("helpdesk.toWaiting"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "hold"} onClose={close} title={t("card.hold")}>
        <div className="space-y-3">
          <p className="text-sm text-muted">{t("card.holdHint")}</p>
          <Field label={t("card.holdReason")}>
            <textarea rows={3} value={text} onChange={(e) => setText(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
          <MinutesInput value={minutes} onChange={setMinutes} />
          {box(() => hold.mutate(undefined), hold.isPending || !text.trim(), t("card.hold"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "merge"} onClose={close} title={t("card.merge")}>
        <div className="space-y-3">
          <p className="text-sm text-muted">{t("card.mergeHint")}</p>
          <Field label={t("card.mergeInto")}>
            <Input inputMode="numeric" value={parent} onChange={(e) => setParent(e.target.value.replace(/\D/g, ""))} />
          </Field>
          {box(() => merge.mutate(undefined), merge.isPending || !parent || Number(parent) === n, t("card.merge"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "transfer"} onClose={close} title={t("helpdesk.transfer")}>
        <div className="space-y-3">
          <Field label={t("helpdesk.engineer")}>
            <Select value={target} onChange={(e) => setTarget(e.target.value)}>
              <option value="">{t("helpdesk.choose")}</option>
              {engineers.data?.map((e) => (
                <option key={e.user_id} value={e.user_id}>
                  {e.name} · {e.lines.join("/").toUpperCase()} · {e.load}
                  {e.absent ? ` · ${t("helpdesk.absent")}` : ""}
                </option>
              ))}
            </Select>
          </Field>
          <Field label={t("helpdesk.line")}>
            <Select value={line} onChange={(e) => setLine(e.target.value)}>
              <option value="l1">L1</option>
              <option value="l2">L2</option>
            </Select>
          </Field>
          <Field label={t("helpdesk.transferReason")}>
            <textarea rows={3} value={text} onChange={(e) => setText(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
          {box(() => transfer.mutate(undefined), transfer.isPending || !target || !text.trim(), t("helpdesk.transfer"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "major"} onClose={close} title={t("helpdesk.makeMajor")}>
        <div className="space-y-3">
          <Field label={t("helpdesk.majorTitle")}>
            <Input value={title} onChange={(e) => setTitle(e.target.value)} />
          </Field>
          <Field label={t("helpdesk.majorBody")}>
            <textarea rows={3} value={text} onChange={(e) => setText(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
          {box(() => major.mutate(undefined), major.isPending || !title.trim(), t("helpdesk.publish"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "link"} onClose={close} title={t("helpdesk.link")}>
        <div className="space-y-3">
          <Field label={t("helpdesk.parentNumber")} hint={t("helpdesk.parentHint")}>
            <Input inputMode="numeric" value={parent} onChange={(e) => setParent(e.target.value.replace(/\D/g, ""))} />
          </Field>
          {box(() => link.mutate(undefined), link.isPending, t("common.save"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "verify"} onClose={close} title={t("helpdesk.verifyIdentity")}>
        <div className="space-y-3">
          <p className="text-sm text-muted">{t("helpdesk.verifyHint")}</p>
          <PersonPicker value={person} onPick={setPerson} />
          {box(() => verify.mutate(undefined), verify.isPending || !person, t("helpdesk.confirmIdentity"))}
        </div>
      </Dialog>
      <Dialog open={dialog === "notResolved"} onClose={close} title={t("helpdesk.notResolved")}>
        <div className="space-y-3">
          <Field label={t("helpdesk.whatsWrong")}>
            <textarea rows={3} value={text} onChange={(e) => setText(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
          </Field>
          {box(() => notResolved.mutate(undefined), notResolved.isPending || !text.trim(), t("helpdesk.returnToWork"))}
        </div>
      </Dialog>
    </div>
  );
}

function Properties({ d, lang }: { d: TicketDetail; lang: string }) {
  const { t } = useTranslation();
  const remote = useModule("remote");
  const directory = useModule("directory");
  const x: TicketHead = d.ticket;
  const qc = useQueryClient();
  const cats = useQuery({ queryKey: ["categories"], queryFn: () => ok<Category[]>(api.GET("/api/helpdesk/categories")), enabled: d.can_work });
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")), enabled: d.can_work });
  const update = useMutation({
    mutationFn: (b: { priority?: string; category_id?: string; site_id?: string }) => ok(api.PUT("/api/helpdesk/tickets/{number}", { params: { path: { number: x.number } }, body: { version: x.version, ...b } })),
    meta: { toast: t("common.saved") },
    onSettled: () => void qc.invalidateQueries({ queryKey: ["ticket", x.number] }),
  });
  const codes = useCloseCodes();
  const [newProblem, setNewProblem] = useState(false);
  const contact = x.unverified_contact as { name?: string; issue?: string } | null;
  const answers = Object.entries((x.form_answers ?? {}) as Record<string, unknown>);
  return (
    <Card className="p-4">
      <dl>
        <Prop label={t("helpdesk.requester")}>
          <span className="font-medium">{x.requester_name ?? "—"}</span> {x.requester_vip && <Badge tone="accent">VIP</Badge>}{" "}
          {x.unverified && <Badge tone="bad">{t("helpdesk.unverified")}</Badge>}
        </Prop>
        {x.requester_email && <Prop label="Email">{x.requester_email}</Prop>}
        {x.contact_phone && <Prop label={t("helpdesk.phone")}><span className="font-mono">{x.contact_phone}</span></Prop>}
        {x.contact_room && <Prop label={t("helpdesk.room")}>{x.contact_room}</Prop>}
        {x.computer_name && <Prop label={t("helpdesk.computer")}><span className="font-mono">{x.computer_name}</span></Prop>}
        {contact?.issue && <Prop label={t("helpdesk.issue")}>{t(`helpdesk.issues.${contact.issue}`, { defaultValue: contact.issue })}</Prop>}
        <Prop label={t("helpdesk.site")}>
          {d.can_work && sites.data ? (
            <Select aria-label={t("helpdesk.site")} value={x.site_id ?? ""} onChange={(e) => update.mutate({ site_id: e.target.value })}>
              {!x.site_id && <option value="">—</option>}
              {sites.data.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            </Select>
          ) : (x.site_name ?? "—")}
        </Prop>
        <Prop label={t("helpdesk.category")}>
          {d.can_work && cats.data ? (
            <Select aria-label={t("helpdesk.category")} value={x.category_id ?? ""} onChange={(e) => update.mutate({ category_id: e.target.value })}>
              {!cats.data.some((c) => c.id === x.category_id) && <option value={x.category_id ?? ""}>{x.category_name ?? "—"}</option>}
              {cats.data.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </Select>
          ) : (x.category_name ?? "—")}
        </Prop>
        {answers.map(([k, v]) => <Prop key={k} label={k}>{String(v)}</Prop>)}
        {d.can_work && (
          <Prop label={t("helpdesk.priorityLabel")}>
            <Select aria-label={t("helpdesk.priorityLabel")} value={x.priority} onChange={(e) => update.mutate({ priority: e.target.value })}>
              {PRIORITIES.map((p) => <option key={p} value={p}>{dyn(t, `helpdesk.priority.${p}`)}</option>)}
            </Select>
          </Prop>
        )}
        {d.can_work && (
          <Prop label={t("card.impactUrgency")}>
            <ImpactUrgency d={d} />
          </Prop>
        )}
        <Prop label={t("helpdesk.assignee")}>
          <AssigneeControl d={d} />
        </Prop>
        {(d.can_work || x.group_name) && (
          <Prop label={t("card.group")}>
            <GroupControl d={d} />
          </Prop>
        )}
        {x.closure_code && <Prop label={t("card.closeCode")}>{closeCodeLabel(codes.data, x.closure_code)}</Prop>}
        {d.can_work && x.work_minutes > 0 && <Prop label={t("card.workTotal")}>{t("card.minutesValue", { m: x.work_minutes })}</Prop>}
        <Prop label={t("helpdesk.line")}>{x.line.toUpperCase()}</Prop>
        <Prop label={t("helpdesk.channelLabel")}>{dyn(t, `helpdesk.channel.${x.channel}`)}</Prop>
        {x.created_by_name && <Prop label={t("helpdesk.createdBy")}>{x.created_by_name}</Prop>}
        <Prop label={t("helpdesk.createdAt")}>{formatDate(x.created_at, lang)}</Prop>
        {x.taken_at && <Prop label={t("helpdesk.takenAt")}>{formatDate(x.taken_at, lang)}</Prop>}
        {x.resolved_at && <Prop label={t("helpdesk.resolvedAt")}>{formatDate(x.resolved_at, lang)}</Prop>}
        {x.auto_close_at && x.status === "resolved" && <Prop label={t("helpdesk.autoCloseAt")}>{formatDate(x.auto_close_at, lang)}</Prop>}
        {x.reopened_count > 0 && <Prop label={t("helpdesk.reopenedCount")}>{x.reopened_count}</Prop>}
        {x.parent_number && <Prop label={t("helpdesk.parent")}><TicketLink number={x.parent_number} className="text-accent">#{x.parent_number}</TicketLink></Prop>}
        {x.follow_up_number && <Prop label={t("helpdesk.followUpOf")}><TicketLink number={x.follow_up_number} className="text-accent">#{x.follow_up_number}</TicketLink></Prop>}
        {x.problem_number && <Prop label={t("itil.kind.problem")}><TicketLink number={x.problem_number} className="text-accent">#{x.problem_number}</TicketLink></Prop>}
      </dl>
      {d.can_work && !x.problem_number && (x.kind === "incident" || x.kind === "request") && (
        <>
          <Button variant="ghost" className="mt-1 min-h-7 px-2 text-xs" onClick={() => setNewProblem(true)}>
            {t("itil.fromIncident")}
          </Button>
          {newProblem && <NewItilDialog kind="problem" open incident={x.number} title={x.title} onClose={() => setNewProblem(false)} />}
        </>
      )}
      {update.error && <ErrorBox error={update.error} />}
      {d.can_work && remote && <TicketRemote ticket={x.number} personId={x.requester_person_id} computer={x.computer_name} />}
      <SlaPanel d={d} lang={lang} />
      <AccessPanel d={d} />
      {directory && <AccountPanel d={d} />}
      <Checklist d={d} />
      {directory && <Operations d={d} />}
      <Watchers d={d} />
      <Objects d={d} />
      <Links d={d} />
      {d.children.length > 0 && (
        <div className="mt-3 border-t border-line pt-2 text-sm">
          <div className="mb-1 font-medium">{t("helpdesk.linkedTickets")}</div>
          <ul className="space-y-1">
            {d.children.map((c) => (
              <li key={c.number} className="flex items-center gap-2">
                <TicketLink number={c.number} className="font-mono text-xs text-accent">#{c.number}</TicketLink>
                <span className="min-w-0 flex-1 truncate">{c.title}</span>
                <StatusBadge status={c.status} />
              </li>
            ))}
          </ul>
        </div>
      )}
    </Card>
  );
}

export function TicketPage() {
  const { t, i18n } = useTranslation();
  const { number } = useParams({ from: "/tickets/$number" });
  const n = Number(number);
  const q = useQuery({ queryKey: ["ticket", n], queryFn: () => ok<TicketDetail>(api.GET("/api/helpdesk/tickets/{number}", { params: { path: { number: n } } })) });
  const [focus, setFocus] = useState(0);
  const [tab, setTab] = useState<"public" | "internal">("public");
  const [dialog, setDialog] = useState<DialogKind>(null);
  const canWork = q.data?.can_work;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (ignoreHotkey(e)) return;
      if (e.key === "r") {
        e.preventDefault();
        setTab("public");
        setFocus(Date.now());
      }
      if (e.key === "n" && canWork) {
        e.preventDefault();
        setTab("internal");
        setFocus(Date.now());
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [canWork]);
  if (q.error) return <ErrorBox error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner />;
  const d = q.data;
  const x = d.ticket;
  return (
    <div className="mx-auto max-w-6xl">
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <span className="font-mono text-sm text-muted">#{x.number}</span>
        <h1 className="min-w-0 text-lg font-semibold break-words">{x.title}</h1>
        <StatusBadge status={x.status} />
        <PriorityBadge priority={x.priority} />
        {d.can_work && <SlaBadge state={x.sla_state} />}
        {x.is_major && <Badge tone="bad">{t("helpdesk.major")}</Badge>}
        {(x.kind === "problem" || x.kind === "change") && <Badge tone="accent">{dyn(t, `itil.kind.${x.kind}`)}</Badge>}
      </div>
      <div className="mb-4">
        <Actions d={d} dialog={dialog} setDialog={setDialog} />
      </div>
      <div className="grid gap-4 lg:grid-cols-[1fr_20rem]">
        <div className="min-w-0 space-y-3">
          <Card className="p-4">
            <RichText text={x.description || t("helpdesk.noDescription")} />
            <AttachmentList items={d.attachments} text={x.description} />
          </Card>
          {d.can_work && (x.kind === "problem" || x.kind === "change") && <ItilPanel number={x.number} kind={x.kind} closed={x.status === "closed" || x.status === "canceled"} />}
          {d.can_work && d.feed.length > 0 && <AssistSummary key={x.number} number={x.number} />}
          <ul className="space-y-2">
            {d.feed.map((f) => <FeedEntry key={f.id} item={f} lang={i18n.language} />)}
          </ul>
          <Rating d={d} />
          <Reply key={d.ticket.number} d={d} focus={focus} tab={tab} setTab={setTab} />
        </div>
        <Properties d={d} lang={i18n.language} />
      </div>
    </div>
  );
}
