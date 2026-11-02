//! abdns : DNS chiffré du poste, avec son propre serveur DoH d'abord, des secours chiffrés dans
//! l'ordre, et le DNS du réseau seulement en dernier recours (portails captifs).
//!
//! Deux programmes, comme `sshd` et `ssh` : `abdnsd`, le service (administrateur), et `abdns`,
//! l'outil de l'utilisateur, qui le pilote par le canal de `control`.

pub mod config;
pub mod control;
pub mod dns;
pub mod doh;
pub mod guard;
pub mod i18n;
pub mod logbook;
pub mod platform;
pub mod relay;
