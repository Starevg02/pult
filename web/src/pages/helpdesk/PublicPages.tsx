// Страницы без входа: «Не могу войти» и ответ по ссылке из письма.
import { useMutation, useQuery } from "@tanstack/react-query";
import { useParams } from "@tanstack/react-router";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type TokenInfo } from "../../api/client";
import { Logo } from "../../auth";
import { Button, ErrorBox, Field, Input, Select, Spinner } from "../../ui";

export function LoginHelpForm({ onBack }: { onBack: () => void }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [phone, setPhone] = useState("");
  const [room, setRoom] = useState("");
  const [issue, setIssue] = useState("forgot");
  const [comment, setComment] = useState("");
  const send = useMutation({ mutationFn: () => ok(api.POST("/api/public/login-help", { body: { name, phone, room, issue, comment } })) });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    send.mutate();
  };
  return (
    <div className="flex min-h-screen items-start justify-center px-4 py-[8vh]">
      <div className="w-full max-w-sm rounded-2xl border border-line bg-paper p-8 shadow-sm">
        <Logo />
        <h1 className="mt-5 mb-4 text-xl font-semibold">{t("helpdesk.loginHelp.title")}</h1>
        {send.isSuccess ? (
          <p className="text-sm">{t("helpdesk.loginHelp.sent")}</p>
        ) : (
          <form onSubmit={submit} className="space-y-4">
            <Field label={t("helpdesk.loginHelp.name")}>
              <Input required autoFocus value={name} onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field label={t("helpdesk.phone")}>
              <Input required value={phone} onChange={(e) => setPhone(e.target.value)} />
            </Field>
            <Field label={t("helpdesk.room")}>
              <Input value={room} onChange={(e) => setRoom(e.target.value)} />
            </Field>
            <Field label={t("helpdesk.loginHelp.what")}>
              <Select value={issue} onChange={(e) => setIssue(e.target.value)}>
                {(["forgot", "locked", "expired", "other"] as const).map((k) => (
                  <option key={k} value={k}>
                    {t(`helpdesk.issues.${k}`)}
                  </option>
                ))}
              </Select>
            </Field>
            <Field label={t("helpdesk.loginHelp.comment")}>
              <Input value={comment} onChange={(e) => setComment(e.target.value)} />
            </Field>
            {send.error && <ErrorBox error={send.error} />}
            <Button type="submit" variant="primary" className="w-full py-2.5" disabled={send.isPending}>
              {t("helpdesk.send")}
            </Button>
          </form>
        )}
        <button type="button" className="mt-4 text-sm text-accent hover:underline" onClick={onBack}>
          {t("helpdesk.loginHelp.back")}
        </button>
      </div>
    </div>
  );
}

export function TicketResponsePage() {
  const { t } = useTranslation();
  const { token } = useParams({ from: "/ticket-response/$token" });
  const wanted = new URLSearchParams(window.location.search).get("a");
  const [comment, setComment] = useState("");
  const [choice, setChoice] = useState<"yes" | "no">(wanted === "no" ? "no" : "yes");
  const info = useQuery({ queryKey: ["token", token], queryFn: () => ok<TokenInfo>(api.GET("/api/public/ticket-response/{token}", { params: { path: { token } } })), retry: false });
  // Действие — только по нажатию: почтовые сканеры сами открывают ссылки из писем.
  const send = useMutation({ mutationFn: () => ok(api.POST("/api/public/ticket-response/{token}", { params: { path: { token } }, body: { accepted: choice === "yes", comment } })) });
  return (
    <div className="flex min-h-screen items-start justify-center px-4 py-[10vh]">
      <div className="w-full max-w-md rounded-2xl border border-line bg-paper p-8 shadow-sm">
        <Logo />
        {info.isPending ? (
          <Spinner />
        ) : send.isSuccess ? (
          <p className="mt-5 text-sm">{choice === "yes" ? t("helpdesk.response.thanks") : t("helpdesk.response.returned")}</p>
        ) : !info.data?.valid ? (
          <p className="mt-5 text-sm">{t("helpdesk.response.invalid")}</p>
        ) : (
          <div className="mt-5 space-y-4">
            <h1 className="text-lg font-semibold">
              #{info.data.number} {info.data.title}
            </h1>
            <p className="text-sm text-muted">{t("helpdesk.response.question")}</p>
            <div className="flex gap-2">
              <Button variant={choice === "yes" ? "primary" : "ghost"} onClick={() => setChoice("yes")}>
                {t("helpdesk.yesResolved")}
              </Button>
              <Button variant={choice === "no" ? "primary" : "ghost"} onClick={() => setChoice("no")}>
                {t("helpdesk.notResolved")}
              </Button>
            </div>
            {choice === "no" && (
              <Field label={t("helpdesk.whatsWrong")}>
                <textarea autoFocus rows={3} value={comment} onChange={(e) => setComment(e.target.value)} className="w-full rounded-lg border border-control bg-paper px-3 py-2 text-sm" />
              </Field>
            )}
            {send.error && <ErrorBox error={send.error} />}
            <Button variant="primary" className="w-full py-2.5" disabled={send.isPending || (choice === "no" && !comment.trim())} onClick={() => send.mutate()}>
              {t("helpdesk.response.confirm")}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
