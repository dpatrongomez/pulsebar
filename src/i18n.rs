//! Localisation (12 languages). Follows the system language unless overridden in settings.
//! Strings are kept in a compact table; unknown keys fall back to English, then to the key.

pub const LANGS: [&str; 12] = ["en", "es", "de", "fr", "it", "nl", "pt-BR", "sv", "uk", "ja", "ko", "zh"];

type Row = (&'static str, [&'static str; 12]);

#[rustfmt::skip]
const TABLE: &[Row] = &[
    ("tab.overview",   ["Overview","Resumen","Übersicht","Aperçu","Panoramica","Overzicht","Visão geral","Översikt","Огляд","概要","개요","概览"]),
    ("tab.cpu",        ["CPU","CPU","CPU","CPU","CPU","CPU","CPU","CPU","ЦП","CPU","CPU","CPU"]),
    ("tab.memory",     ["Memory","Memoria","Speicher","Mémoire","Memoria","Geheugen","Memória","Minne","Пам’ять","メモリ","메모리","内存"]),
    ("tab.disk",       ["Disk","Disco","Festplatte","Disque","Disco","Schijf","Disco","Disk","Диск","ディスク","디스크","磁盘"]),
    ("tab.network",    ["Network","Red","Netzwerk","Réseau","Rete","Netwerk","Rede","Nätverk","Мережа","ネットワーク","네트워크","网络"]),
    ("tab.gpu",        ["GPU","GPU","GPU","GPU","GPU","GPU","GPU","GPU","ГП","GPU","GPU","GPU"]),
    ("tab.neural",     ["Neural","Neural","Neural","Neural","Neural","Neural","Neural","Neural","Нейро","ニューラル","뉴럴","神经网络"]),
    ("tab.battery",    ["Battery","Batería","Batterie","Batterie","Batteria","Batterij","Bateria","Batteri","Батарея","バッテリー","배터리","电池"]),
    ("tab.sound",      ["Sound","Sonido","Ton","Son","Suono","Geluid","Som","Ljud","Звук","サウンド","사운드","声音"]),
    ("tab.bluetooth",  ["Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","Bluetooth","蓝牙"]),
    ("tab.projects",   ["Projects","Proyectos","Projekte","Projets","Progetti","Projecten","Projetos","Projekt","Проєкти","プロジェクト","프로젝트","项目"]),
    ("tab.agents",     ["Agents","Agentes","Agenten","Agents","Agenti","Agents","Agentes","Agenter","Агенти","エージェント","에이전트","智能体"]),
    ("tab.containers", ["Containers","Contenedores","Container","Conteneurs","Container","Containers","Contêineres","Containrar","Контейнери","コンテナ","컨테이너","容器"]),
    ("tab.history",    ["History","Histórico","Verlauf","Historique","Cronologia","Geschiedenis","Histórico","Historik","Історія","履歴","기록","历史"]),
    ("tab.settings",   ["Settings","Ajustes","Einstellungen","Réglages","Impostazioni","Instellingen","Ajustes","Inställningar","Налаштування","設定","설정","设置"]),
    ("now",            ["Now","Ahora","Jetzt","Maintenant","Ora","Nu","Agora","Nu","Зараз","現在","현재","当前"]),
    ("avg_today",      ["Average today","Media de hoy","Heute im Schnitt","Moyenne du jour","Media di oggi","Gemiddeld vandaag","Média de hoje","Snitt i dag","Середнє сьогодні","今日の平均","오늘 평균","今日平均"]),
    ("peak",           ["Peak","Pico","Spitze","Pic","Picco","Piek","Pico","Topp","Пік","ピーク","최고","峰值"]),
    ("top_apps",       ["Top Apps","Apps principales","Top-Apps","Apps principales","App principali","Top-apps","Principais apps","Toppappar","Топ програм","上位アプリ","상위 앱","热门应用"]),
    ("processes",      ["processes","procesos","Prozesse","processus","processi","processen","processos","processer","процесів","プロセス","프로세스","个进程"]),
    ("busiest",        ["Busiest Right Now","Más activas ahora","Gerade am aktivsten","Les plus actives","Più attive ora","Nu het drukst","Mais ativos agora","Mest aktiva nu","Найактивніші зараз","現在最も負荷が高い","지금 가장 바쁜 앱","当前最繁忙"]),
    ("quit",           ["Quit","Salir","Beenden","Quitter","Esci","Stop","Encerrar","Avsluta","Завершити","終了","종료","退出"]),
    ("force_quit",     ["Force Quit","Forzar salida","Sofort beenden","Forcer à quitter","Uscita forzata","Forceer stop","Forçar encerrar","Tvångsavsluta","Примусово завершити","強制終了","강제 종료","强制退出"]),
    ("inspect",        ["Inspect","Inspeccionar","Untersuchen","Inspecter","Ispeziona","Inspecteren","Inspecionar","Inspektera","Переглянути","調査","검사","检查"]),
    ("cancel",         ["Cancel","Cancelar","Abbrechen","Annuler","Annulla","Annuleer","Cancelar","Avbryt","Скасувати","キャンセル","취소","取消"]),
    ("stop",           ["Stop…","Detener…","Stoppen…","Arrêter…","Ferma…","Stoppen…","Parar…","Stoppa…","Зупинити…","停止…","중지…","停止…"]),
    ("stop_all",       ["Stop All…","Detener todo…","Alle stoppen…","Tout arrêter…","Ferma tutto…","Alles stoppen…","Parar tudo…","Stoppa alla…","Зупинити все…","すべて停止…","모두 중지…","全部停止…"]),
    ("reset_layout",   ["Reset layout","Restablecer diseño","Layout zurücksetzen","Réinitialiser la disposition","Ripristina layout","Indeling herstellen","Redefinir layout","Återställ layout","Скинути розкладку","レイアウトをリセット","레이아웃 재설정","重置布局"]),
    ("search.placeholder", ["Search apps, processes, PID, port…","Buscar apps, procesos, PID, puerto…","Apps, Prozesse, PID, Port suchen…","Rechercher apps, processus, PID, port…","Cerca app, processi, PID, porta…","Zoek apps, processen, PID, poort…","Buscar apps, processos, PID, porta…","Sök appar, processer, PID, port…","Пошук програм, процесів, PID, порту…","アプリ・プロセス・PID・ポートを検索…","앱, 프로세스, PID, 포트 검색…","搜索应用、进程、PID、端口…"]),
    ("search.no_matches", ["No matches","Sin resultados","Keine Treffer","Aucun résultat","Nessun risultato","Geen resultaten","Nenhum resultado","Inga träffar","Нічого не знайдено","一致なし","일치 항목 없음","无匹配项"]),
    ("search.apps", ["Apps","Apps","Apps","Apps","App","Apps","Apps","Appar","Застосунки","アプリ","앱","应用"]),
    ("search.more", ["more","más","weitere","de plus","altri","meer","mais","till","ще","件","개 더","更多"]),
];

fn index_of(lang: &str) -> usize {
    let l = lang.replace('_', "-");
    if let Some(i) = LANGS.iter().position(|x| x.eq_ignore_ascii_case(&l)) {
        return i;
    }
    let base = l.split('-').next().unwrap_or("en");
    match base {
        "pt" => 6,
        "zh" => 11,
        _ => LANGS.iter().position(|x| x.split('-').next() == Some(base)).unwrap_or(0),
    }
}

/// Detect the macOS UI language (first of AppleLanguages), falling back to $LANG.
pub fn system_language() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = s.split('"').nth(1) {
            return first.to_string();
        }
    }
    std::env::var("LANG").ok().and_then(|l| l.split('.').next().map(str::to_string)).unwrap_or_else(|| "en".into())
}

#[derive(Debug, Clone)]
pub struct I18n {
    idx: usize,
}

impl I18n {
    pub fn new(lang: Option<&str>) -> Self {
        let l = lang.map(str::to_string).unwrap_or_else(system_language);
        Self { idx: index_of(&l) }
    }
    pub fn lang(&self) -> &'static str {
        LANGS[self.idx]
    }
    pub fn t(&self, key: &str) -> String {
        TABLE.iter().find(|(k, _)| *k == key).map(|(_, v)| v[self.idx].to_string()).unwrap_or_else(|| key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookups() {
        assert_eq!(I18n::new(Some("es-ES")).t("tab.projects"), "Proyectos");
        assert_eq!(I18n::new(Some("pt-PT")).t("now"), "Agora");
        assert_eq!(I18n::new(Some("zh-Hans-CN")).t("tab.memory"), "内存");
        assert_eq!(I18n::new(Some("xx")).t("peak"), "Peak");
        assert_eq!(I18n::new(Some("en")).t("missing.key"), "missing.key");
    }
}
