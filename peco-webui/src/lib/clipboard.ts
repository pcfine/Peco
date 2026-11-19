/**
 * 复制文本到剪贴板，成功返回 `true`。
 *
 * 先走异步 Clipboard API；它只在安全上下文可用，而用户常从
 * `http://<LAN-IP>:6971` 打开这个界面 —— 那里 `navigator.clipboard` 是
 * `undefined`，所以必须留一条 `execCommand` 的旧路径。
 *
 * 两条路都失败返回 `false`，不抛异常：调用方只需要「成没成」这一个信息，
 * 自己去决定怎么提示，不该在这里再包一层 try/catch。
 */
export async function copyText(text: string): Promise<boolean> {
  if (typeof navigator.clipboard?.writeText === "function") {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // 权限被拒、文档未聚焦都会走到这里：兜底路径还有一次机会。
    }
  }
  return copyViaTextarea(text);
}

function copyViaTextarea(text: string): boolean {
  // 临时节点会把焦点抢过来，撤掉后焦点掉回 body：键盘用户按回车复制完就得
  // 重新 Tab 一遍。先记下当前焦点，收尾时交还。
  const prev = document.activeElement;
  const textarea = document.createElement("textarea");
  textarea.value = text;
  // fixed + 透明：不参与布局，也不会把视口滚到文档末尾。
  textarea.style.position = "fixed";
  textarea.style.top = "0";
  textarea.style.left = "0";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  try {
    // 只有 select() 不够：部分浏览器与 iOS 上不给节点焦点就没有选区，
    // execCommand("copy") 会复制到空白。
    textarea.focus();
    textarea.select();
    textarea.setSelectionRange(0, text.length);
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    textarea.remove();
    if (prev instanceof HTMLElement && prev.isConnected) {
      prev.focus({ preventScroll: true });
    }
  }
}
