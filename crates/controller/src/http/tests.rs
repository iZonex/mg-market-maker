    use super::*;

    #[test]
    fn legacy_config_maps_numeric_knobs() {
        let (k, v) = legacy_config_to_variable("Gamma", "0.02").unwrap();
        assert_eq!(k, "gamma");
        assert_eq!(v, serde_json::json!("0.02"));

        let (k, v) = legacy_config_to_variable("MinSpreadBps", "5").unwrap();
        assert_eq!(k, "min_spread_bps");
        assert_eq!(v, serde_json::json!("5"));

        let (k, v) = legacy_config_to_variable("NumLevels", "3").unwrap();
        assert_eq!(k, "num_levels");
        assert_eq!(v, serde_json::json!(3));
    }

    #[test]
    fn legacy_config_maps_booleans_both_spellings() {
        let (k, v) = legacy_config_to_variable("MomentumEnabled", "true").unwrap();
        assert_eq!(k, "momentum_enabled");
        assert_eq!(v, serde_json::json!(true));

        let (_, v) = legacy_config_to_variable("MomentumEnabled", "1").unwrap();
        assert_eq!(v, serde_json::json!(true));

        let (_, v) = legacy_config_to_variable("MomentumEnabled", "false").unwrap();
        assert_eq!(v, serde_json::json!(false));
    }

    #[test]
    fn legacy_config_maps_pause_resume() {
        let (k, v) = legacy_config_to_variable("PauseQuoting", "").unwrap();
        assert_eq!(k, "paused");
        assert_eq!(v, serde_json::json!(true));

        let (k, v) = legacy_config_to_variable("ResumeQuoting", "").unwrap();
        assert_eq!(k, "paused");
        assert_eq!(v, serde_json::json!(false));
    }

    #[test]
    fn legacy_config_unknown_field_returns_none() {
        assert!(legacy_config_to_variable("SomeFutureKnob", "0.5").is_none());
        // Numeric field with bad value also returns None.
        assert!(legacy_config_to_variable("NumLevels", "not-a-number").is_none());
    }
