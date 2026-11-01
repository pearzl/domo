/// 复制文本到系统剪贴板：优先异步剪贴板 API，不可用时回退到选区复制。
/// 返回是否复制成功，调用方据此决定是否显示“已复制”反馈。
export async function writeClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // 浏览器拒绝剪贴板权限时继续尝试选区复制。
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const copied = document.execCommand("copy");
    document.body.removeChild(area);
    return copied;
  } catch {
    return false;
  }
}
