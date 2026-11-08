import "./styles/app.css";
import { showShelf } from "./app";
import { initSettings } from "./core/settings";

// 设置存在 SQLite 里，先读回来再挂书架，避免先闪一下默认主题/字体
void initSettings().finally(showShelf);
