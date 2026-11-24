//! Planes y derecho a cada canal de aviso.
//!
//! # Por qué el plan gratuito no está mutilado
//!
//! Esta es una aplicación de emergencia. Dejar sin aviso a quien no paga, cuando
//! avisarle por Web Push no cuesta nada, sería indefendible si alguien resultara
//! herido. Así que **el plan gratuito recibe todos los niveles por Web Push**, que
//! es el canal sin coste por mensaje, y lo que el plan de pago añade es el SMS
//! para el nivel `evacuate`: el canal que sigue funcionando cuando el teléfono no
//! tiene datos, que es justo lo que pasa en una emergencia y lo que cuesta dinero
//! por cada envío.
//!
//! Eso mantiene el coste variable acotado y justifica la cuota por algo real, sin
//! condicionar el aviso básico a haber pagado.
//!
//! # Cómo se otorga
//!
//! Esta capa solo **lee** el derecho. Mientras no haya pasarela de pago, un
//! administrador lo concede desde `/api/v1/admin/entitlements/{usuario}`, y queda
//! auditado. Cuando se integre la pasarela, su webhook escribirá en esta misma
//! colección, así que el motor de alertas no tendrá que cambiar.

use super::Channel;
use crate::{db, error::Result, hazards::Level};
use mongodb::{
    Database,
    bson::{DateTime, Document, doc},
};

/// Plan de una cuenta.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Plan {
    /// Web Push en todos los niveles. Sin coste por mensaje.
    Free,
    /// Añade SMS en el nivel `evacuate`.
    Paid,
}
impl Plan {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Paid => "paid",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "free" => Some(Self::Free),
            "paid" => Some(Self::Paid),
            _ => None,
        }
    }
    /// Canales a los que este plan da derecho en este nivel.
    pub fn channels(self, level: Level) -> &'static [Channel] {
        match (self, level) {
            // Nadie recibe nada por un nivel que no avisa.
            (_, Level::None) => &[],
            // El SMS cuesta por mensaje, así que se reserva para lo grave y para
            // quien lo paga.
            (Self::Paid, Level::Evacuate) => &[Channel::Push, Channel::Sms],
            _ => &[Channel::Push],
        }
    }
}

/// Plan vigente de una cuenta.
///
/// Un plan de pago caducado vale lo mismo que no tenerlo: se devuelve `Free`. La
/// comprobación es por fecha y no por una bandera, para que olvidarse de
/// desactivar algo no regale envíos de pago indefinidamente.
pub async fn plan_for(db: &Database, owner: &str) -> Result<Plan> {
    let found = db
        .collection::<Document>("entitlements")
        .find_one(doc! {"_id": owner})
        .await?;
    let Some(d) = found else {
        return Ok(Plan::Free);
    };
    let plan = d
        .get_str("plan")
        .ok()
        .and_then(Plan::parse)
        .unwrap_or(Plan::Free);
    if plan == Plan::Free {
        return Ok(Plan::Free);
    }
    let valid = d
        .get_datetime("valid_until")
        .is_ok_and(|until| *until > DateTime::now());
    Ok(if valid { plan } else { Plan::Free })
}

/// Concede o retira un plan.
///
/// `days` cero retira el plan de pago dejando la cuenta en el gratuito, en lugar
/// de borrar el registro, para que quede rastro de que existió.
pub async fn grant(
    db: &Database,
    actor: &str,
    owner: &str,
    plan: Plan,
    days: i64,
    payment_reference: Option<&str>,
) -> Result<Document> {
    db::audit(db, actor, "entitlement_grant", owner).await?;
    let valid_until = db::after(days * 86_400);
    let fields = doc! {
        "plan": plan.as_str(),
        "valid_until": valid_until,
        // Referencia del pago que lo originó. Mientras la concesión sea manual
        // queda nula, y al integrar la pasarela guardará su identificador, que es
        // lo que permite conciliar y resolver una reclamación.
        "payment_reference": payment_reference,
        "granted_by": actor,
        "updated": db::now(),
    };
    db.collection::<Document>("entitlements")
        .update_one(doc! {"_id": owner}, doc! {"$set": &fields})
        .upsert(true)
        .await?;
    let mut out = fields;
    out.insert("_id", owner);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_still_gets_warned_about_everything_that_matters() {
        // La regla ética del módulo, escrita como prueba: ningún nivel deja a una
        // cuenta gratuita sin aviso.
        for level in [Level::Info, Level::Prepare, Level::Evacuate] {
            assert!(
                Plan::Free.channels(level).contains(&Channel::Push),
                "el plan gratuito se quedó sin aviso en {}",
                level.as_str()
            );
        }
    }

    #[test]
    fn sms_is_only_for_paid_evacuation() {
        assert_eq!(
            Plan::Paid.channels(Level::Evacuate),
            &[Channel::Push, Channel::Sms]
        );
        // Ni el plan de pago gasta un SMS en algo que no es una evacuación.
        assert_eq!(Plan::Paid.channels(Level::Prepare), &[Channel::Push]);
        assert_eq!(Plan::Paid.channels(Level::Info), &[Channel::Push]);
        assert!(!Plan::Free.channels(Level::Evacuate).contains(&Channel::Sms));
    }

    #[test]
    fn a_level_that_does_not_warn_sends_nothing() {
        assert!(Plan::Free.channels(Level::None).is_empty());
        assert!(Plan::Paid.channels(Level::None).is_empty());
    }

    #[test]
    fn plan_round_trips_through_its_text() {
        for plan in [Plan::Free, Plan::Paid] {
            assert_eq!(Plan::parse(plan.as_str()), Some(plan));
        }
        assert_eq!(Plan::parse("premium"), None);
    }
}
