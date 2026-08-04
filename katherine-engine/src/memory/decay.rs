// memory/decay.rs — 分源衰减曲线。
// 衰减在检索时计算，不写入存储（P2）。
// 与桌面设计稿 Katherine-v3-设计稿.md §四 保持一致。

/// 衰减曲线类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecayCurve {
    /// 幂律：R(t) = (1 + t/s)^(-a)。永不完全遗忘。
    /// 用于 corrections, decisions。
    PowerLaw { scale_days: f64, exponent: f64 },
    /// 指数：R(t) = e^(-λt)。快速衰减。
    /// 用于 engine_log, dialogue。
    Exponential { lambda: f64 },
    /// 对数：R(t) = 1 / ln(e + t/s)。极缓慢衰减。
    /// 用于 insights。
    Logarithmic { scale_days: f64 },
    /// 不衰减：R(t) = 1.0。
    /// 用于 identity_anchor。
    None,
}

impl DecayCurve {
    /// 从 source 和 importance 推导衰减曲线。
    /// 设计原则 P4：分源衰减。
    pub fn from_source(source: &str, _importance: f32) -> Self {
        match source {
            "selena_correction" => DecayCurve::PowerLaw {
                scale_days: 30.0,
                exponent: 1.5,
            },
            "my_decision" => DecayCurve::PowerLaw {
                scale_days: 60.0,
                exponent: 1.0,
            },
            "insight" => DecayCurve::Logarithmic {
                scale_days: 7.0,
            },
            "identity_anchor" => DecayCurve::None,
            "engine" => DecayCurve::Exponential { lambda: 0.05 },
            // dialogue, raw_dialogue, 其他 →
            _ => DecayCurve::Exponential { lambda: 0.02 },
        }
    }

    /// 曲线名称，存入 events.decay_curve。
    pub fn as_str(&self) -> &'static str {
        match self {
            DecayCurve::PowerLaw { .. } => "power_law",
            DecayCurve::Exponential { .. } => "exponential",
            DecayCurve::Logarithmic { .. } => "logarithmic",
            DecayCurve::None => "none",
        }
    }

    /// 从字符串解析。
    pub fn from_str(s: &str) -> Self {
        match s {
            "power_law" => DecayCurve::PowerLaw {
                scale_days: 30.0,
                exponent: 1.5,
            },
            "exponential" => DecayCurve::Exponential { lambda: 0.05 },
            "logarithmic" => DecayCurve::Logarithmic {
                scale_days: 7.0,
            },
            _ => DecayCurve::None,
        }
    }

    /// 计算衰减因子 r ∈ [0, 1]。
    /// days_since = 从 last_retrieved 到现在的天数。0 表示刚被检索过。
    pub fn compute(&self, days_since: f64) -> f64 {
        match self {
            DecayCurve::PowerLaw {
                scale_days,
                exponent,
            } => (1.0 + days_since / scale_days).powf(-exponent),
            DecayCurve::Exponential { lambda } => (-lambda * days_since).exp(),
            DecayCurve::Logarithmic { scale_days } => {
                1.0 / (std::f64::consts::E + days_since / scale_days).ln()
            }
            DecayCurve::None => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_law_never_zero() {
        let c = DecayCurve::PowerLaw { scale_days: 30.0, exponent: 1.5 };
        assert!(c.compute(0.0) > 0.99);
        assert!(c.compute(365.0) > 0.0); // 一年后仍 > 0
    }

    #[test]
    fn exponential_decays_fast() {
        let c = DecayCurve::Exponential { lambda: 0.05 };
        assert!(c.compute(0.0) > 0.99);
        assert!(c.compute(90.0) < 0.02); // 三个月后接近零
    }

    #[test]
    fn none_always_one() {
        assert!((DecayCurve::None.compute(1000.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn from_source_correction_is_power_law() {
        let c = DecayCurve::from_source("selena_correction", 0.9);
        assert!(matches!(c, DecayCurve::PowerLaw { .. }));
    }

    #[test]
    fn from_source_engine_is_exponential() {
        let c = DecayCurve::from_source("engine", 0.6);
        assert!(matches!(c, DecayCurve::Exponential { .. }));
    }
}
