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

    pub fn clear_body(&self, reason: &str, network: &str, servers: &str) -> String {
        self.pick(
            format!("{reason} on {network}: using the network DNS ({servers}). Every name looked up will be reported."),
            format!("{reason} sur {network} : le DNS du réseau ({servers}) est utilisé. Chaque domaine demandé vous sera signalé."),
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

    pub fn stopped_title(&self) -> String {
        self.pick("abdns stopped".into(), "abdns arrêté".into())
    }

    pub fn stopped_body(&self, error: &str) -> String {
        self.pick(format!("Relay unavailable, network DNS restored: {error}"), format!("Relais impossible, DNS du réseau remis : {error}"))
    }

    pub fn unknown_network(&self) -> String {
        self.pick("unknown network".into(), "réseau inconnu".into())
    }

    pub fn mode_set(&self, mode: &str) -> String {
        self.pick(
            format!("mode {mode}: applied at the next check (10 to 30 s)"),
            format!("mode {mode} : appliqué au prochain contrôle (10 à 30 s)"),
        )
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
