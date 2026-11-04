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
    ("login.section",          ["Startup","Inicio","Start","Démarrage","Avvio","Opstarten","Inicialização","Start","Запуск","起動","시작","启动"]),
    ("login.toggle",           ["Open at login","Abrir al iniciar sesión","Beim Anmelden öffnen","Ouvrir à la connexion","Apri al login","Openen bij inloggen","Abrir ao iniciar sessão","Öppna vid inloggning","Відкривати під час входу","ログイン時に開く","로그인 시 열기","登录时打开"]),
    ("login.requires_approval",["Requires approval","Requiere aprobación","Erfordert Genehmigung","Approbation requise","Richiede approvazione","Goedkeuring vereist","Requer aprovação","Kräver godkännande","Потрібне схвалення","承認が必要","승인 필요","需要批准"]),
    ("login.open_settings",    ["Open Login Items…","Abrir Ítems de inicio…","Anmeldeobjekte öffnen…","Ouvrir les éléments de connexion…","Apri Elementi login…","Open inlogonderdelen…","Abrir Itens de início…","Öppna Inloggningsobjekt…","Відкрити елементи входу…","ログイン項目を開く…","로그인 항목 열기…","打开登录项…"]),
    ("login.unbundled",        ["Only available in the installed PulseBar app.","Solo disponible en la app PulseBar instalada.","Nur in der installierten PulseBar-App verfügbar.","Disponible uniquement dans l’app PulseBar installée.","Disponibile solo nell’app PulseBar installata.","Alleen beschikbaar in de geïnstalleerde PulseBar-app.","Disponível apenas no app PulseBar instalado.","Endast tillgängligt i den installerade PulseBar-appen.","Доступно лише в установленій програмі PulseBar.","インストール済みの PulseBar アプリでのみ利用できます。","설치된 PulseBar 앱에서만 사용할 수 있습니다.","仅在已安装的 PulseBar 应用中可用。"]),
    ("login.error",            ["Couldn’t change the login item","No se pudo cambiar el ítem de inicio","Anmeldeobjekt konnte nicht geändert werden","Impossible de modifier l’élément de connexion","Impossibile modificare l’elemento login","Kan het inlogonderdeel niet wijzigen","Não foi possível alterar o item de início","Det gick inte att ändra inloggningsobjektet","Не вдалося змінити елемент входу","ログイン項目を変更できませんでした","로그인 항목을 변경할 수 없습니다","无法更改登录项"]),
    ("login.offer",            ["Open PulseBar automatically when you log in?","¿Abrir PulseBar automáticamente al iniciar sesión?","PulseBar beim Anmelden automatisch öffnen?","Ouvrir PulseBar automatiquement à la connexion ?","Aprire PulseBar automaticamente al login?","PulseBar automatisch openen bij inloggen?","Abrir o PulseBar automaticamente ao iniciar sessão?","Öppna PulseBar automatiskt vid inloggning?","Автоматично відкривати PulseBar під час входу?","ログイン時に PulseBar を自動的に開きますか？","로그인 시 PulseBar를 자동으로 열까요?","登录时自动打开 PulseBar？"]),
    ("login.not_now",          ["Not now","Ahora no","Nicht jetzt","Pas maintenant","Non ora","Niet nu","Agora não","Inte nu","Не зараз","今はしない","나중에","暂不"]),
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
        assert_eq!(I18n::new(Some("es")).t("login.toggle"), "Abrir al iniciar sesión");
        assert_eq!(I18n::new(Some("en")).t("login.requires_approval"), "Requires approval");
    }
}
