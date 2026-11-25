use super::*;

#[test]
fn escrow_error_codes_round_trip() {
    for &error in ALL_ESCROW_ERRORS {
        let code = error as u32;
        assert_eq!(u32::from(error), code);
        assert_eq!(EscrowError::try_from(code), Ok(error));
        assert_eq!(
            EscrowError::try_from(soroban_sdk::Error::from(error)),
            Ok(error)
        );
        assert_eq!(
            EscrowError::try_from(soroban_sdk::InvokeError::from(error)),
            Ok(error)
        );
    }
}

#[test]
fn escrow_error_codes_reject_unknown_values() {
    for code in [0, 34, 999, u32::MAX] {
        assert_eq!(EscrowError::try_from(code), Err(code));
        let error = soroban_sdk::Error::from_contract_error(code);
        assert_eq!(EscrowError::try_from(error), Err(error));
        let error = soroban_sdk::InvokeError::Contract(code);
        assert_eq!(EscrowError::try_from(error), Err(error));
    }
    assert_eq!(
        EscrowError::try_from(soroban_sdk::InvokeError::Abort),
        Err(soroban_sdk::InvokeError::Abort)
    );
}

#[test]
fn escrow_error_collision_repairs_preserve_existing_codes() {
    for (error, code) in [
        (EscrowError::AlreadyFunded, 28),
        (EscrowError::TimelockExtensionNotFound, 450),
        (EscrowError::InvalidTrancheSchedule, 456),
        (EscrowError::ValueConservationViolation, 461),
        (EscrowError::MilestoneNotFound, 471),
        (EscrowError::SameLedgerManipulationDetected, 612),
        (EscrowError::InvalidTtlExtension, 491),
        (EscrowError::AlreadyShipped, 492),
        (EscrowError::CourierPubkeyNotSet, 493),
        (EscrowError::ExceedsTotalEscrowAmount, 494),
        (EscrowError::UnfundedCancellationNotAllowed, 495),
        (EscrowError::DefiPoolDetached, 496),
    ] {
        assert_eq!(error as u32, code);
        assert_eq!(EscrowError::try_from(code), Ok(error));
    }
}

#[test]
fn all_escrow_error_codes_are_unique() {
    let mut codes: std::vec::Vec<_> = ALL_ESCROW_ERRORS
        .iter()
        .map(|error| *error as u32)
        .collect();
    codes.sort_unstable();
    for pair in codes.windows(2) {
        assert_ne!(pair[0], pair[1], "duplicate EscrowError code: {}", pair[0]);
    }
}
