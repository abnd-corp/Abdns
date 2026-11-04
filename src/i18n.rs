//! Textes montrés à l'utilisateur (notifications, commandes), en anglais ou en français.

use serde::Deserialize;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Fr,
}

pub struct Text(pub Lang);

impl Text {
    fn pick(&self, en: String, fr: String) -> String {
        match self.0 {
            Lang::En => en,
            Lang::Fr => fr,
        }
    }

    pub fn clear_title(&self) -> String {
        self.pick("DNS in clear".into(), "DNS en clair".into())
    }

    pub fn no_encrypted(&self) -> String {
        self.pick("No encrypted DNS answers".into(), "Aucun DNS chiffré ne répond".into())
    }

    pub fn forced_local(&self) -> String {
        self.pick("Local mode forced".into(), "Mode local imposé".into())
    }

    pub fn clear_body(&self, reason: &str, network: &str) -> String {
        self.pick(
            format!("{reason} on {network}: using the network DNS. Names looked up will be reported."),
            format!("{reason} sur {network} : le DNS du réseau est utilisé. Les domaines demandés vous seront signalés."),
        )
    }

    pub fn local_disabled_body(&self, network: &str) -> String {
        self.pick(
            format!("No encrypted DNS answers on {network}, and the local fallback is disabled."),
            format!("Aucun DNS chiffré ne répond sur {network}, et le dernier recours local est désactivé."),
        )
    }

    pub fn names_title(&self) -> String {
        self.pick("Looked up in clear".into(), "Demandé en clair".into())
    }

    pub fn names(&self, shown: &str, more: usize) -> String {
        match (self.0, more) {
            (_, 0) => shown.to_owned(),
            (Lang::En, 1) => format!("{shown} and 1 other"),
            (Lang::En, n) => format!("{shown} and {n} others"),
            (Lang::Fr, 1) => format!("{shown} et 1 autre"),
            (Lang::Fr, n) => format!("{shown} et {n} autres"),
        }
    }

    pub fn restored_title(&self) -> String {
        self.pick("Encrypted DNS restored".into(), "DNS chiffré rétabli".into())
    }

    pub fn restored_body(&self, minutes: u64, names: usize) -> String {
        self.pick(
            format!("After {minutes} min in clear; {names} name(s) sent to the network DNS."),
            format!("Après {minutes} min en clair ; {names} domaine(s) demandé(s) au réseau."),
        )
    }

    pub fn portal_title(&self) -> String {
        self.pick("Captive portal".into(), "Portail détecté".into())
    }

    pub fn portal_body(&self) -> String {
        self.pick("Opening the network sign-in page.".into(), "Ouverture de la page de connexion du réseau.".into())
    }

    pub fn still_clear_title(&self) -> String {
        self.pick("Still in clear".into(), "Toujours en clair".into())
    }

    pub fn still_clear_body(&self, minutes: u64, network: &str) -> String {
        self.pick(
            format!("For {minutes} min on {network}: encrypted DNS is still unreachable."),
            format!("Depuis {minutes} min sur {network} : le DNS chiffré reste injoignable."),
        )
    }

    pub fn fallback_title(&self) -> String {
        self.pick("Fallback server in use".into(), "Secours utilisé".into())
    }

    pub fn fallback_body(&self, primary: &str, fallback: &str) -> String {
        self.pick(format!("{primary} does not answer: using {fallback}."), format!("{primary} ne répond pas : {fallback} prend le relais."))
    }

    pub fn primary_back_title(&self) -> String {
        self.pick("Main server back".into(), "Serveur principal rétabli".into())
    }

    pub fn primary_back_body(&self, primary: &str) -> String {
        self.pick(format!("{primary} answers again."), format!("{primary} répond de nouveau."))
    }

    pub fn reloaded_title(&self) -> String {
        self.pick("Configuration reloaded".into(), "Configuration rechargée".into())
    }

    pub fn reloaded_body(&self, servers: &str) -> String {
        self.pick(format!("Servers, in order: {servers}."), format!("Serveurs, dans l'ordre : {servers}."))
    }

    pub fn invalid_config_title(&self) -> String {
        self.pick("Configuration ignored".into(), "Configuration ignorée".into())
    }

    pub fn invalid_config_body(&self, error: &str) -> String {
        self.pick(format!("The previous one stays in use: {error}"), format!("L'ancienne reste en service : {error}"))
    }

    pub fn stopped_title(&self) -> String {
        self.pick("abdns stopped".into(), "abdns arrêté".into())
    }

    pub fn stopped_body(&self, error: &str) -> String {
        self.pick(format!("Relay unavailable, network DNS restored: {error}"), format!("Relais impossible, DNS du réseau remis : {error}"))
    }

    pub fn crashed_title(&self) -> String {
        self.pick("abdns stopped abruptly".into(), "abdns arrêté net".into())
    }

    pub fn crashed_body(&self) -> String {
        self.pick(
            "Network DNS restored, in clear, until abdns restarts (within seconds).".into(),
            "DNS du réseau remis, en clair, jusqu'à la relance d'abdns (quelques secondes).".into(),
        )
    }

    pub fn unusable_title(&self) -> String {
        self.pick("Server set aside".into(), "Serveur mis de côté".into())
    }

    pub fn unusable_body(&self, problems: &str) -> String {
        self.pick(format!("{problems}. The other servers answer."), format!("{problems}. Les autres serveurs répondent."))
    }

    pub fn unknown_network(&self) -> String {
        self.pick("unknown network".into(), "réseau inconnu".into())
    }

    pub fn mode_set(&self, mode: &str) -> String {
        self.pick(format!("mode {mode}: applied within a few seconds"), format!("mode {mode} : appliqué dans quelques secondes"))
    }

    pub fn token_saved(&self, path: &str) -> String {
        self.pick(format!("token saved to {path}"), format!("jeton enregistré dans {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural() {
        assert_eq!(Text(Lang::En).names("a, b", 0), "a, b");
        assert_eq!(Text(Lang::En).names("a", 1), "a and 1 other");
        assert_eq!(Text(Lang::Fr).names("a", 3), "a et 3 autres");
    }
}
