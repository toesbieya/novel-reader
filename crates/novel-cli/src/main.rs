use std::error::Error;
use std::path::Path;
use std::time::Instant;

use novel_core::Document;

fn main() {
    if let Err(e) = run() {
        eprintln!("错误：{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("gen") => gen_cmd(&args),
        Some("info") => info_cmd(&args),
        Some("toc") => toc_cmd(&args),
        Some("read") => read_cmd(&args),
        Some("search") => search_cmd(&args),
        Some("bench") => bench_cmd(&args),
        Some("selftest") => selftest_cmd(&args),
        Some("lib-books") => lib_books_cmd(&args),
        Some("lib-import") => lib_import_cmd(&args),
        Some("lib-folder") => lib_folder_cmd(&args),
        Some("backfill") => backfill_cmd(&args),
        _ => {
            help();
            Ok(())
        }
    }
}

fn help() {
    println!(
        "\
novel-cli —— novel-core 压测工具

  gen    <输出路径> [MB]        生成合成中文小说用于压测（GB18030）
  info   <文件>                 打印编码 / 章节数 / 大小
  toc    <文件> [前N条]         打印目录
  read   <文件> <章节下标>      读一章并打印
  search <文件> <关键词> [上限] 全文搜索
  bench  <文件> [关键词]        性能穿刺：跑全部红线指标
  selftest                     数据层验收：目录深度 / 零副本 / 进度持久化
  lib-books <db>               列出书架内容
  backfill <db>                给老库补章节字数（进度按字数加权用）
"
    );
}

fn arg(args: &[String], i: usize) -> Option<&str> {
    args.get(i).map(|s| s.as_str())
}

fn info_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(path) = arg(args, 1) else {
        println!("用法：novel-cli info <文件>");
        return Ok(());
    };
    let t = Instant::now();
    let doc = Document::open(Path::new(path))?;
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;

    println!("标题   {}", doc.title());
    println!("格式   {}", doc.format());
    println!("编码   {}", doc.encoding_name());
    println!("大小   {:.2} MB", doc.file_size() as f64 / 1048576.0);
    println!("章节   {}", doc.chapter_count());
    println!("打开   {open_ms:.2} ms");
    Ok(())
}

fn toc_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(path) = arg(args, 1) else {
        println!("用法：novel-cli toc <文件> [前N条]");
        return Ok(());
    };
    let n: usize = arg(args, 2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let doc = Document::open(Path::new(path))?;
    for ch in doc.chapters().iter().take(n) {
        println!("{:>6}  {}", ch.index, ch.title);
    }
    println!("…… 共 {} 章", doc.chapter_count());
    Ok(())
}

fn read_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(path) = arg(args, 1) else {
        println!("用法：novel-cli read <文件> <章节下标>");
        return Ok(());
    };
    let idx: usize = arg(args, 2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let doc = Document::open(Path::new(path))?;
    let t = Instant::now();
    let text = doc.read_chapter(idx)?;
    println!("读取耗时 {:.3} ms，{} 字\n", t.elapsed().as_secs_f64() * 1000.0, text.chars().count());
    let head: String = text.chars().take(800).collect();
    println!("{head}");
    Ok(())
}

fn search_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(path) = arg(args, 1) else {
        println!("用法：novel-cli search <文件> <关键词> [上限]");
        return Ok(());
    };
    let Some(q) = arg(args, 2) else {
        println!("请给一个关键词");
        return Ok(());
    };
    let limit: usize = arg(args, 3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let doc = Document::open(Path::new(path))?;
    let r = doc.search(q, limit);
    println!(
        "命中 {} 处（展示 {} 条），耗时 {:.2} ms",
        r.total,
        r.hits.len(),
        r.elapsed_us as f64 / 1000.0
    );
    for h in &r.hits {
        println!("  [{}] 第 {} 字  {}", h.chapter_title, h.char_offset, h.snippet);
    }
    Ok(())
}

fn bench_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(path) = arg(args, 1) else {
        println!("用法：novel-cli bench <文件> [关键词]");
        return Ok(());
    };
    let p = Path::new(path);

    let t = Instant::now();
    let doc = Document::open(p)?;
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let n = doc.chapter_count();

    println!("文件         {}", p.display());
    println!("格式 / 编码  {} / {}", doc.format(), doc.encoding_name());
    println!("大小         {:.2} MB", doc.file_size() as f64 / 1048576.0);
    println!("章节         {n}");
    println!();
    println!("打开 + 建目录  {open_ms:>8.2} ms   红线 400");

    // 取目录本身是从内存切片返回，量的是"UI 拿到目录"的调用成本
    let t = Instant::now();
    let toc = doc.chapters();
    let n2 = toc.len();
    println!(
        "取目录         {:>8.4} ms   红线 50    {n2} 项",
        t.elapsed().as_secs_f64() * 1000.0
    );

    // 随机读 200 章
    let mut rng = Rng::new(7);
    let t = Instant::now();
    let mut chars = 0usize;
    let mut worst = 0f64;
    for _ in 0..200 {
        let i = (rng.next() as usize) % n;
        let s0 = Instant::now();
        let s = doc.read_chapter(i)?;
        let dt = s0.elapsed().as_secs_f64() * 1000.0;
        if dt > worst {
            worst = dt;
        }
        chars += s.chars().count();
    }
    let total_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "随机读 200 章   {total_ms:>8.2} ms   平均 {:.3} ms / 最慢 {worst:.3} ms / {chars} 字",
        total_ms / 200.0
    );

    // 跳到 90% 的阅读进度
    let idx = ((n as f64) * 0.9) as usize;
    let t = Instant::now();
    let _ = doc.read_chapter(idx)?;
    println!("跳到 90% 进度   {:>8.3} ms   红线 30", t.elapsed().as_secs_f64() * 1000.0);

    // 全文搜索
    let q = arg(args, 2).unwrap_or("他说道");
    let t = Instant::now();
    let r = doc.search(q, 200);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    if doc.format() == "epub" {
        // 首次搜索可能在建缓存（冷缓存 ~180ms），也可能在读磁盘缓存（~25ms）
        println!("全文搜索「{q}」{ms:>6.2} ms   首次（可能建缓存）   命中 {} 处", r.total);
    } else {
        println!("全文搜索「{q}」{ms:>6.2} ms   红线 120   命中 {} 处", r.total);
    }
    for h in r.hits.iter().take(3) {
        println!("  · [{}] 第 {} 字  {}", h.chapter_title, h.char_offset, h.snippet);
    }

    // EPUB：上面的首次搜索已经把纯文本装进内存（并落盘），第二次起量的是缓存命中
    if doc.format() == "epub" {
        let t = Instant::now();
        let r2 = doc.search(q, 200);
        let hit_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("EPUB 缓存搜索    {hit_ms:>8.2} ms   红线 10   命中 {} 处", r2.total);
    }

    Ok(())
}

fn gen_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = arg(args, 1).unwrap_or("samples/bench.txt").to_string();
    let mb: usize = arg(args, 2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let p = Path::new(&path);
    if let Some(dir) = p.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }

    let t = Instant::now();
    let (bytes, chapters) = synth(mb * 1024 * 1024);
    std::fs::write(p, &bytes)?;
    println!(
        "已生成 {}：{:.2} MB，{chapters} 章，耗时 {:.0} ms",
        p.display(),
        bytes.len() as f64 / 1048576.0,
        t.elapsed().as_millis()
    );
    Ok(())
}

// ---------------------------------------------------------------- 合成语料

const POOL: &str = "的一是了我不人在他有这个上们来到时大地为子中你说生国年着就那和要她出也得里后自以会家可下而过天去能对小多然于心学么之都好看起发当没成只如事把还用第样道想作种开美总从无情己面最女但现前些所同日手又行意动方期它头经长儿回位分爱老因很给名法间斯知世什两次使身者被高已亲其进此话常与活正感";

const TITLES: &[&str] = &[
    "风起", "夜行", "旧梦", "归途", "山雨", "断剑", "残阳", "远客", "孤舟", "春深",
    "长歌", "暗涌", "落雪", "问心", "无名", "边城", "故人", "青锋", "月下", "尘缘",
];

fn synth(target: usize) -> (Vec<u8>, usize) {
    let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
    let mut out: Vec<u8> = Vec::with_capacity(target + 65536);
    let mut chapter = 1usize;

    while out.len() < target {
        let title = format!("第{}章 {}\n\n", cn_num(chapter), TITLES[rng.next() as usize % TITLES.len()]);
        push_gb(&mut out, &title);

        for line in 0..90 {
            push_gb(&mut out, &sentence(&mut rng, line));
            out.push(b'\n');
        }
        out.push(b'\n');
        chapter += 1;
    }

    (out, chapter - 1)
}

fn sentence(rng: &mut Rng, line: usize) -> String {
    // 故意埋两条误判诱饵：正文里出现「第一章」和以「第」开头的普通句子
    if line == 20 {
        return "他在心里默念着第一章的内容，却怎么也记不起来，只好作罢。".to_string();
    }
    if line == 55 {
        return "第一次见到她的时候，他还是个孩子，什么都不懂。".to_string();
    }

    let n = 20 + (rng.next() as usize % 30);
    let mut s = String::with_capacity(n * 3 + 4);
    for _ in 0..n {
        let i = (rng.next() as usize) % POOL.chars().count();
        s.push(POOL.chars().nth(i).unwrap());
    }
    s.push('。');
    s
}

fn push_gb(out: &mut Vec<u8>, s: &str) {
    let (b, _, _) = encoding_rs::GB18030.encode(s);
    out.extend_from_slice(&b);
}

fn cn_num(mut n: usize) -> String {
    const D: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    const U: [&str; 4] = ["", "十", "百", "千"];
    if n == 0 {
        return "零".to_string();
    }
    let mut ds = Vec::new();
    while n > 0 {
        ds.push(n % 10);
        n /= 10;
    }
    let mut s = String::new();
    let mut pending_zero = false;
    for (i, &d) in ds.iter().enumerate().rev() {
        if d == 0 {
            if !s.is_empty() {
                pending_zero = true;
            }
            continue;
        }
        if pending_zero {
            s.push('零');
            pending_zero = false;
        }
        s.push_str(D[d]);
        s.push_str(U[i]);
    }
    if let Some(rest) = s.strip_prefix("一十") {
        s = format!("十{rest}");
    }
    s
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x2545_F491_4F6C_DD1D } else { seed })
    }

    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }
}

// ---------------------------------------------------------------- 数据层验收

struct Checker {
    ok: usize,
    fail: usize,
}

impl Checker {
    fn new() -> Self {
        Self { ok: 0, fail: 0 }
    }

    fn check(&mut self, cond: bool, name: &str) {
        if cond {
            self.ok += 1;
            println!("  [ok]   {name}");
        } else {
            self.fail += 1;
            println!("  [FAIL] {name}");
        }
    }
}

/// 用真实文件读写跑一遍数据层，验证三条硬约束：
/// 目录深度 ≤5、删除不碰原文件、进度跨进程持久化。
fn selftest_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    use novel_core::store::Library;

    let root = match arg(args, 1) {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::temp_dir().join("novel-reader-selftest"),
    };
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let db = root.join("library.db");

    let mut c = Checker::new();

    let (bytes, expect_chapters) = synth(400 * 1024);
    let txt = root.join("测试小说.txt");
    std::fs::write(&txt, &bytes)?;
    c.check(expect_chapters >= 5, &format!("合成语料 {expect_chapters} 章"));

    let mut lib = Library::open(&db)?;

    // 1. 目录树规则
    let mut parent: Option<i64> = None;
    let mut root_id = 0i64;
    for lvl in 1..=5 {
        let id = lib.create_folder(parent, &format!("第{lvl}级"))?;
        if lvl == 1 {
            root_id = id;
        }
        parent = Some(id);
    }
    c.check(true, "连续创建 5 级目录");
    c.check(lib.create_folder(parent, "第6级").is_err(), "第 6 级被拒绝");
    c.check(lib.create_folder(None, "第1级").is_err(), "同级重名被拒绝");
    c.check(lib.create_folder(None, "  ").is_err(), "空目录名被拒绝");

    // 2. 导入与去重
    let out = lib.import_book(&txt, parent)?;
    c.check(out.chapter_count as usize == expect_chapters, "入库章节数与切分结果一致");
    let dup = lib.import_book(&txt, None)?;
    c.check(dup.already_exists && dup.id == out.id, "重复导入不产生第二条记录");

    // 换一个文件，才能真正走到外键校验（同一文件会在去重处早返）
    let txt2 = root.join("另一本.txt");
    std::fs::write(&txt2, &bytes)?;
    c.check(lib.import_book(&txt2, Some(9999)).is_err(), "导入到不存在的目录被拒绝");

    // 3. 目录 / 正文 / 搜索
    c.check(lib.chapters(out.id)?.len() == expect_chapters, "目录从库里直接读出，不重扫文件");
    let ch = lib.read_chapter(out.id, 0)?;
    c.check(!ch.text.trim().is_empty(), "读取正文非空");
    let r = lib.search(out.id, "的", 5)?;
    c.check(r.total > 0 && r.hits.len() <= 5, &format!("搜索命中 {} 处并截断到 5 条", r.total));

    // 3.5 字数与按字数加权的进度
    let total_chars: i64 = lib.conn().query_row(
        "SELECT COALESCE(SUM(char_len), 0) FROM chapter WHERE book_id = ?1",
        [out.id],
        |r| r.get(0),
    )?;
    c.check(total_chars > 0, &format!("导入时写入了章节字数（共 {total_chars} 字）"));

    let empty_lens: i64 = lib.conn().query_row(
        "SELECT COUNT(*) FROM chapter WHERE book_id = ?1 AND char_len <= 0",
        [out.id],
        |r| r.get(0),
    )?;
    c.check(empty_lens == 0, "每一章都算出了字数");

    let mid = (expect_chapters / 2) as i64;
    lib.save_progress(out.id, mid, 0)?;
    let done_chars: i64 = lib.conn().query_row(
        "SELECT COALESCE(SUM(char_len), 0) FROM chapter WHERE book_id = ?1 AND idx < ?2",
        [out.id, mid],
        |r| r.get(0),
    )?;
    let expect_progress = done_chars as f64 / total_chars as f64;
    let summary = lib.list_books()?.into_iter().find(|b| b.id == out.id);
    let got = summary.as_ref().map(|b| b.progress);
    c.check(
        (got.unwrap_or(-1.0) - expect_progress).abs() < 1e-9,
        &format!("进度按字数加权（{:.4}）", expect_progress),
    );
    // 书架卡片的「第 N 章」直接用 chapter_idx，不能从百分比反推——
    // 列下标错一位会静默显示成别的章节，所以这里钉一下
    c.check(
        summary.as_ref().and_then(|b| b.last_chapter) == Some(mid),
        &format!("书架摘要带回章节位置（第 {} 章）", mid + 1),
    );
    c.check(
        summary.as_ref().is_some_and(|b| b.added_at > 0),
        "书架摘要带回入库时间",
    );

    // 模拟升级前的老库：把 char_len 清零后回填
    lib.conn().execute("UPDATE chapter SET char_len = 0 WHERE book_id = ?1", [out.id])?;
    let legacy = lib.list_books()?.into_iter().find(|b| b.id == out.id).map(|b| b.progress);
    let legacy_aspect = mid as f64 / expect_chapters as f64;
    c.check(
        (legacy.unwrap_or(-1.0) - legacy_aspect).abs() < 1e-9,
        "没有字数时回退到按章节估算",
    );

    let fixed = lib.backfill_char_lens()?;
    c.check(fixed >= 1, &format!("老库回填 {fixed} 本"));
    let after_fix = lib.list_books()?.into_iter().find(|b| b.id == out.id).map(|b| b.progress);
    c.check(
        (after_fix.unwrap_or(-1.0) - expect_progress).abs() < 1e-9,
        "回填后进度回到加权口径",
    );

    // 3.6 章节长短悬殊时，两种口径必须拉开差距
    // 合成语料每章字数接近，光靠它验证不出加权是否生效，这里手搓一本 100 字 + 900 字的书。
    lib.conn().execute(
        "INSERT INTO book (title, format, file_path, file_size, file_mtime, encoding,
                           chapter_count, missing, added_at)
         VALUES ('长短不一', 'txt', 'X:/_selftest/uneven.txt', 1000, 0, 'UTF-8', 2, 0, 0)",
        [],
    )?;
    let uneven = lib.conn().last_insert_rowid();
    lib.conn().execute(
        "INSERT INTO chapter (book_id, idx, title, start, end, char_len)
         VALUES (?1, 0, '第1章', 0, 100, 100), (?1, 1, '第2章', 100, 1000, 900)",
        [uneven],
    )?;
    lib.save_progress(uneven, 1, 0)?; // 读到第 2 章开头 = 100/1000 字
    let weighted = lib.list_books()?.into_iter().find(|b| b.id == uneven).map(|b| b.progress);
    c.check(
        (weighted.unwrap_or(-1.0) - 0.1).abs() < 1e-9,
        &format!("长短章节按字数加权（应为 10%，实为 {:.1}%）", weighted.unwrap_or(-1.0) * 100.0),
    );
    c.check(
        (weighted.unwrap_or(-1.0) - 0.5).abs() > 0.1,
        "与按章节序号算的 50% 明显不同",
    );
    lib.delete_book(uneven)?;

    // 4. 进度跨进程持久化
    lib.save_progress(out.id, 10, 500)?;
    if let Some(p) = lib.progress(out.id)? {
        c.check(p.chapter_index == 10 && p.char_offset == 500, "进度写入并读回");
    } else {
        c.check(false, "进度写入并读回");
    }
    drop(lib);

    let mut lib = Library::open(&db)?;
    match lib.progress(out.id)? {
        Some(p) => c.check(p.chapter_index == 10, "重开数据库后进度仍在"),
        None => c.check(false, "重开数据库后进度仍在"),
    }

    // 5. 删除书籍：只删记录
    lib.delete_book(out.id)?;
    c.check(txt.is_file(), "删除书籍后原文件仍在");
    c.check(lib.list_books()?.iter().all(|b| b.id != out.id), "书架记录已移除");

    // 6. 删除目录：级联移除记录，同样不碰原文件
    lib.import_book(&txt, parent)?;
    let d = lib.delete_folder(root_id)?;
    c.check(d.folders_removed == 5, &format!("级联删除 5 层目录（实际 {}）", d.folders_removed));
    c.check(d.books_removed == 1, &format!("目录内书籍一并移除（实际 {}）", d.books_removed));
    c.check(txt.is_file(), "删除目录后原文件仍在");

    // 7. 文件失效标记与恢复
    let b3 = lib.import_book(&txt, None)?;
    let moved = root.join("挪走了.txt");
    std::fs::rename(&txt, &moved)?;
    lib.refresh_missing()?;
    let missing = lib
        .list_books()?
        .into_iter()
        .find(|b| b.id == b3.id)
        .map(|b| b.missing)
        .unwrap_or(false);
    c.check(missing, "原文件被移走后标记 missing");

    std::fs::rename(&moved, &txt)?;
    lib.refresh_missing()?;
    let restored = lib
        .list_books()?
        .into_iter()
        .find(|b| b.id == b3.id)
        .map(|b| !b.missing)
        .unwrap_or(false);
    c.check(restored, "文件回来后恢复可用");
    c.check(txt.is_file(), "全流程结束后原文件完好。");

    // 8. 同级目录排序（书架拖拽排序的后端）
    let a = lib.create_folder(None, "排序甲")?;
    let b = lib.create_folder(None, "排序乙")?;
    let cc = lib.create_folder(None, "排序丙")?;
    let roots = |lib: &Library| -> Vec<i64> {
        lib.list_folders()
            .unwrap()
            .into_iter()
            .filter(|f| f.parent_id.is_none())
            .map(|f| f.id)
            .collect()
    };
    c.check(roots(&lib) == vec![a, b, cc], "新建目录按创建顺序排列");

    lib.reorder_folder(cc, a, false)?;
    c.check(roots(&lib) == vec![cc, a, b], "拖到前一位后顺序正确");

    lib.reorder_folder(cc, b, true)?;
    c.check(roots(&lib) == vec![a, b, cc], "拖到后一位后顺序正确");

    let inner = lib.create_folder(Some(a), "排序子")?;
    c.check(lib.reorder_folder(inner, b, false).is_err(), "跨层级排序被拒绝");

    // sort 被重压成 0..n-1，反复拖拽不会让数值发散
    let scattered: i64 = lib.conn().query_row(
        "SELECT COUNT(*) FROM folder WHERE parent_id IS NULL AND sort NOT IN (0, 1, 2)",
        [],
        |r| r.get(0),
    )?;
    c.check(scattered == 0, "sort 值保持连续紧凑");

    // 9. 导入文件夹：同名分组目录合并（同一父级下复用，不同父级互不影响）
    let (merged_a, created_a) = lib.ensure_folder(None, "合并测试")?;
    let (merged_b, created_b) = lib.ensure_folder(None, "合并测试")?;
    c.check(
        merged_a == merged_b && created_a && !created_b,
        "ensure_folder 同级同名复用",
    );
    let (child_a, child_created_a) = lib.ensure_folder(Some(merged_a), "子分组")?;
    let (child_b, child_created_b) = lib.ensure_folder(Some(merged_a), "子分组")?;
    c.check(
        child_a == child_b && child_created_a && !child_created_b,
        "ensure_folder 子级同名复用",
    );
    let (other, _) = lib.ensure_folder(None, "合并测试二")?;
    c.check(
        lib.ensure_folder(Some(other), "子分组")?.0 != child_a,
        "不同父级下的同名目录互不影响",
    );

    println!("\n通过 {} 项，失败 {} 项", c.ok, c.fail);
    if c.fail > 0 {
        return Err(std::io::Error::other("数据层验收未通过").into());
    }
    Ok(())
}

fn lib_books_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    use novel_core::store::Library;

    let Some(db) = arg(args, 1) else {
        println!("用法：novel-cli lib-books <db>");
        return Ok(());
    };
    let lib = Library::open(Path::new(db))?;
    for f in lib.list_folders()? {
        println!("[目录 {:>3}] parent={:?}  {}", f.id, f.parent_id, f.name);
    }
    for b in lib.list_books()? {
        println!(
            "[书   {:>3}] folder={:?}  {} — {} 章 / {} / {:.0}%{}",
            b.id,
            b.folder_id,
            b.title,
            b.chapter_count,
            b.encoding,
            b.progress * 100.0,
            if b.missing { " / 文件失效" } else { "" }
        );
    }
    Ok(())
}

fn backfill_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    use novel_core::store::Library;

    let Some(db) = arg(args, 1) else {
        println!("用法：novel-cli backfill <db>");
        return Ok(());
    };

    let t0 = Instant::now();
    let mut lib = Library::open(Path::new(db))?;
    let opened = t0.elapsed();

    let t1 = Instant::now();
    let n = lib.backfill_char_lens()?;
    println!(
        "补算 {n} 本的章节字数，耗时 {:.0} ms（其中打开库 {:.0} ms）",
        t1.elapsed().as_millis(),
        opened.as_millis()
    );
    Ok(())
}

fn lib_import_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    use novel_core::store::Library;

    let (Some(db), Some(file)) = (arg(args, 1), arg(args, 2)) else {
        println!("用法：novel-cli lib-import <db> <文件> [目录ID]");
        return Ok(());
    };
    let folder: Option<i64> = arg(args, 3).and_then(|s| s.parse().ok()).filter(|v| *v > 0);

    let mut lib = Library::open(Path::new(db))?;
    let out = lib.import_book(Path::new(file), folder)?;
    println!(
        "#{} {} — {} 章 / {} / {}",
        out.id,
        out.title,
        out.chapter_count,
        out.encoding,
        if out.already_exists { "已在书架" } else { "已导入" }
    );
    Ok(())
}

fn lib_folder_cmd(args: &[String]) -> Result<(), Box<dyn Error>> {
    use novel_core::store::Library;

    let (Some(db), Some(name)) = (arg(args, 1), arg(args, 2)) else {
        println!("用法：novel-cli lib-folder <db> <名称> [父目录ID]");
        return Ok(());
    };
    let parent: Option<i64> = arg(args, 3).and_then(|s| s.parse().ok()).filter(|v| *v > 0);

    let mut lib = Library::open(Path::new(db))?;
    let id = lib.create_folder(parent, name)?;
    println!("目录 #{id} {name}");
    Ok(())
}
