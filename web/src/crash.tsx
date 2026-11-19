// Ошибка при отрисовке: вместо белого экрана — сообщение и кнопка перезагрузки.
import { Component, type ReactNode } from "react";
import i18n from "./i18n";

export class Crash extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: unknown) {
    console.error(error);
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div role="alert" className="mx-auto mt-[15vh] max-w-md rounded-2xl border border-line bg-paper p-8 text-center">
        <p className="mb-4 text-sm">{i18n.t("common.crash")}</p>
        <button type="button" className="rounded-lg bg-ink px-3 py-1.5 text-sm font-medium text-paper" onClick={() => window.location.reload()}>
          {i18n.t("common.reloadPage")}
        </button>
      </div>
    );
  }
}
