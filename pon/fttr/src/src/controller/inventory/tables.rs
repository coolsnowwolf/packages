use crate::pon_engine::omci::{
    mib::{MibReport, TableSnapshot},
    schema::MeRegistry,
    OmciSession,
};
use std::io;

pub(super) fn collect(
    session: &mut OmciSession,
    report: &mut MibReport,
    mut progress: impl FnMut(usize, usize) -> io::Result<()>,
) -> io::Result<()> {
    let attr = MeRegistry::get(171)
        .and_then(|definition| definition.attribute(6))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "VLAN table schema missing"))?;
    let expected = report.entities.iter().filter(|e| e.class_id == 171).count();
    let mut received = 0;
    for entity in report.entities.iter().filter(|e| e.class_id == 171) {
        let result = session.with_timeout(std::time::Duration::from_secs(30), |session| {
            session.get_table(171, entity.entity_id, attr.mask)
        });
        let result = match result {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotConnected | io::ErrorKind::Interrupted
                ) =>
            {
                return Err(error)
            }
            other => other,
        };
        report.tables.push(TableSnapshot::from_result(
            171,
            entity.entity_id,
            attr,
            result,
        ));
        received += 1;
        progress(received, expected)?;
    }
    report.tables_collected = report.tables.iter().all(|table| table.collected);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{
        mib::{MibAssembler, MibRecord},
        session::OmciTransport,
        wire::BaselineFrame,
    };

    struct FailingTable(io::ErrorKind);
    impl OmciTransport for FailingTable {
        fn send(&mut self, _: &BaselineFrame) -> io::Result<()> {
            Err(io::Error::new(self.0, "table read failed"))
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            unreachable!()
        }
    }

    fn uploaded() -> MibReport {
        let mut assembler = MibAssembler::new(3);
        for (sequence, class_id, entity_id) in [(0, 263, 0x8001), (1, 268, 512), (2, 171, 513)] {
            let mut data = [0; 26];
            data[..2].copy_from_slice(&259u16.to_be_bytes());
            assembler.push(MibRecord {
                sequence,
                class_id,
                entity_id,
                attribute_mask: 0x8000,
                data,
            });
        }
        assembler.finish()
    }

    #[test]
    fn table_timeout_preserves_uploaded_ani_and_gem() {
        // Given a complete upload and an optional table that times out.
        let mut report = uploaded();
        let mut session = OmciSession::new(FailingTable(io::ErrorKind::TimedOut));
        // When collecting optional VLAN rules.
        let result = collect(&mut session, &mut report, |_, _| Ok(()));
        // Then the complete upload survives with a localized table error.
        assert!(result.is_ok());
        assert!(report.uploaded_complete);
        assert!(report
            .entities
            .iter()
            .any(|e| e.class_id == 263 && e.entity_id == 0x8001));
        assert!(report
            .entities
            .iter()
            .any(|e| e.class_id == 268 && e.entity_id == 512));
        assert!(!report.tables_collected);
        assert!(!report.tables[0].collected);
        assert!(report.tables[0].error.is_some());
    }

    #[test]
    fn registration_loss_remains_fatal_during_table_read() {
        // Given an upload whose registration disappears during the optional read.
        let mut report = uploaded();
        let mut session = OmciSession::new(FailingTable(io::ErrorKind::NotConnected));
        // When collecting rules, then the caller receives registration loss.
        assert_eq!(
            collect(&mut session, &mut report, |_, _| Ok(()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotConnected
        );
    }
}
