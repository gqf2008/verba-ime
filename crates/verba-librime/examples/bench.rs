//! 自研 verba-pinyin vs Rime（luna_pinyin_simp + octagram 八股文模型）整句首候选对比。
//! 运行（Windows，需 rime.dll + 数据）：
//!   $env:VERBA_RIME_DLL=...; $env:VERBA_RIME_SHARED=...; $env:VERBA_RIME_USER=...;
//!   cargo run -p verba-librime --example bench

use verba_librime::{RimeConfig, RimeEngine};

/// (期望文本, 拼音串) —— 50 句日常对话，含易混淆同音字。
const CASES: &[(&str, &str)] = &[
    ("今天晚上吃什么", "jintianwanshangchishenme"),
    ("我的情感很丰富", "wodeqingganhenfengfu"),
    ("他们很有耐心", "tamenhenyounaixin"),
    ("这个问题很难解决", "zhegewentihennanjiejue"),
    ("外面正在下雨", "waimianzhengzaixiayu"),
    ("电脑桌面很干净", "diannaozhuomianhenganjing"),
    ("阅读理解很重要", "yuedulijiehenzhongyao"),
    ("技术进步很快", "jishujinbuhenkuai"),
    ("学生活动很丰富", "xueshenghuodonghenfengfu"),
    ("今天天气很好", "jintiantianqihenhao"),
    ("我喜欢读书", "woxihuandushu"),
    ("明天要去上班", "mingtianyaoqushangban"),
    ("祝你生日快乐", "zhunishengrikuaile"),
    ("我们一起吃饭", "womenyiqichifan"),
    ("学习需要努力", "xuexixuyaonuli"),
    ("手机没电了", "shoujimeidianle"),
    ("图书馆很安静", "tushuguanhenanjing"),
    ("音乐让人放松", "yinyuerangrenfangsong"),
    ("运动有益健康", "yundongyouyijiankang"),
    ("城市交通很拥堵", "chengshijiaotonghenyongdu"),
    ("他们明天出发", "tamenmingtianchufa"),
    ("会议已经开始了", "huiyiyijingkaishile"),
    ("我们需要合作", "womenxuyaohezuo"),
    ("这个问题很复杂", "zhegewentihenfuza"),
    ("天气越来越冷", "tianqiyuelaiyueleng"),
    ("他的想法很独特", "tadexiangfahendute"),
    ("孩子们在玩游戏", "haizimenzaiwanyouxi"),
    ("老师讲得很清楚", "laoshijiangdehenqingchu"),
    ("请把窗户关上", "qingbachuanghuguanshang"),
    ("我的钱包丢了", "wodeqianbaodiule"),
    ("明天会更好", "mingtianhuigenghao"),
    ("大家一起努力", "dajiayiqinuli"),
    ("我同意你的意见", "wotongyinideyijian"),
    ("科学改变世界", "kexuegaibianshijie"),
    ("历史值得铭记", "lishizhidemingji"),
    ("经济持续发展", "jingjichixufazhan"),
    ("环境需要保护", "huanjingxuyaobaohu"),
    ("文化丰富多彩", "wenhuafengfuduocai"),
    ("朋友之间要信任", "pengyouzhijianyaoxinren"),
    ("时间过得真快", "shijianguodezhenkuai"),
    ("我们晚上见", "womenwanshangjian"),
    ("他的成绩很好", "tadechengjihenhao"),
    ("注意身体健康", "zhuyishentijiankang"),
    ("春天来了", "chuntianlaile"),
    ("树叶变黄了", "shuyebianhuangle"),
    ("他说话很幽默", "tashuohuahenyoumo"),
    ("我的梦想很大", "wodemengxianghenda"),
    ("学习使我快乐", "xuexishiwokuaile"),
    ("感谢你的帮助", "ganxienidebangzhu"),
    ("我们一起努力", "womenyiqinuli"),
];

fn main() {
    let rime = if std::env::var("VERBA_RIME_DLL").is_ok() {
        let cfg = RimeConfig::load(
            &std::path::PathBuf::from(std::env::var("VERBA_RIME_DLL").unwrap()),
            &std::path::PathBuf::from(std::env::var("VERBA_RIME_SHARED").unwrap()),
            &std::path::PathBuf::from(std::env::var("VERBA_RIME_USER").unwrap()),
        );
        match RimeEngine::new(&cfg) {
            Ok(e) => Some(e),
            Err(err) => {
                eprintln!("Rime 加载失败: {err}");
                None
            }
        }
    } else {
        eprintln!("未设置 VERBA_RIME_DLL，跳过 Rime 对比");
        None
    };

    let engine = verba_pinyin::PinyinEngine::new();
    let mut builtin_hit = 0usize;
    let mut rime_hit = 0usize;
    let mut rime_done = false;
    for (i, (expected, py)) in CASES.iter().enumerate() {
        let builtin = engine
            .lookup(py)
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default();
        let b_ok = builtin == *expected;
        if b_ok {
            builtin_hit += 1;
        }
        let mut line = format!(
            "{:>2}. {} | 期望 {} | builtin {} {}",
            i + 1,
            py,
            expected,
            builtin,
            if b_ok { "✓" } else { "✗" }
        );
        if let Some(r) = &rime {
            let rc = r
                .candidates(py, "luna_pinyin_simp", 1)
                .ok()
                .and_then(|c| c.into_iter().next())
                .map(|c| c.text)
                .unwrap_or_default();
            let r_ok = rc == *expected;
            if r_ok {
                rime_hit += 1;
            }
            rime_done = true;
            line.push_str(&format!(" | rime {} {}", rc, if r_ok { "✓" } else { "✗" }));
        }
        println!("{line}");
    }
    let n = CASES.len();
    println!("\n==== 汇总（{} 句） ====", n);
    println!(
        "builtin verba-pinyin 首候选准确率: {}/{} ({:.1}%)",
        builtin_hit,
        n,
        builtin_hit as f64 * 100.0 / n as f64
    );
    if rime_done {
        println!(
            "rime luna_pinyin_simp(octagram) 首候选准确率: {}/{} ({:.1}%)",
            rime_hit,
            n,
            rime_hit as f64 * 100.0 / n as f64
        );
    }
}
