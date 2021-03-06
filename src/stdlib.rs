use error::*;
use parser::parse;
use ast::Exp;
use engine::*;
use eval::*;
use types::*;

pub fn create_ctx(path: &str) -> Ret<Context> {
	let mut ctx = Context::new(path.to_string());
	ctx.add_macro("import", &lib_import)?;
	ctx.add_macro("sup", &lib_sup)?;
	ctx.add_macro("phf", &lib_phf)?;
	ctx.add_macro("gate", &lib_gate)?;
	ctx.add_macro("inv", &lib_inv)?;
	ctx.add_macro("len", &lib_len)?;
	ctx.add_macro("slice", &lib_slice)?;
	ctx.add_macro("weighted", &lib_weighted)?;
	ctx.add_macro("fourier", &lib_fourier)?;
	ctx.add_macro("repeat", &lib_repeat)?;
	ctx.add_macro("fold", &lib_fold)?;
	ctx.add_macro("measure", &lib_measure)?;
	ctx.add_macro("typeof", &lib_typeof)?;
	eval_exp_inline(&parse(r#"
		data Bool = F | T
		data Axis = X | Y | Z
		
		let ((^), (~), (#)) = (sup, phf, measure)
		fn identity(a) = a
		fn (>>)(x, f) = f(x)
		fn (<<)(f, x) = f(x)
		fn (>.)(f, g)(a) = g(f(a))
		fn (<.)(f, g)(a) = f(g(a))
		fn (..)(r)(s) = slice(s, r)
		
		fn map(f)(iter) = fold((), iter, \(xs, x) -> (...xs, f(x)))
		fn filter(f)(iter) = fold((), iter, \(xs, x) -> if f(x) then (...xs, x) else xs)
		
		fn ident {
			F => F,
			T => T,
		}
		fn px {
			F => T,
			T => F,
		}
		fn py {
			F => @[1/2] T,
			T => @[-1/2] F,
		}
		fn pz {
			F => F,
			T => ~T,
		}
		fn had {
			F => F ^ T,
			T => F ^ ~T,
		}
		fn ctrl(g)(c: Bool, x: Bool) = (c, extract c {
			F => x,
			T => g(x),
		})
		let cnot = ctrl(px)
	"#.to_string())?, &mut ctx);
	Ok(ctx)
}

fn lib_import(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	match eval_exp(exp, ctx) {
		RunVal::String(ref s) => ctx.import_eval(s.as_str()),
		_ => err!("Invalid import path"),
	}
}

fn lib_sup(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	Ok(RunVal::State(match eval_exp(exp, ctx) {
		RunVal::Tuple(args) => create_sup(args.into_iter().map(build_state).collect()),
		val => build_state(val),
	}, Type::Any /* TODO infer from arg types */))
}

fn lib_phf(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	let val = eval_exp(exp, ctx);
	let gate = build_gate(&val, ctx)
		.map(|g| RunVal::Gate(g.negate()));
	if let Some(g) = gate {Ok(g)}
	else {
		let (s, t) = build_state_typed(val)?;
		Ok(RunVal::State(s.phase_flip(), t))
	}
}

fn lib_gate(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	let val = eval_exp(exp, ctx);
	Ok(RunVal::Gate(build_gate(&val, ctx).ok_or_else(|| Error(format!("Not a gate: {}", val)))?))
}

fn lib_inv(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	let val = eval_exp(exp, ctx);
	Ok(RunVal::Gate(build_gate(&val, ctx).ok_or_else(|| Error(format!("Not a gate: {}", val)))?
		.inverse()))
}

fn lib_len(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	let val = eval_exp(exp, ctx);
	Ok(RunVal::Index(build_gate(&val, ctx)
		.map(|g| g.len())
		.unwrap_or_else(|| build_state(val).len())))
}

fn lib_slice(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	fn to_slice_params(val: RunVal) -> Ret<(usize, usize)> {
		match val {
			RunVal::Index(n) => Ok((0, n)),
			RunVal::Tuple(args) => {
				if let [RunVal::Index(a), RunVal::Index(b)] = args[..] {
					if a <= b {Ok((a, b))}
					else {err!("Invalid slice arguments: {} > {}", a, b)}
				}
				else {err!("Invalid slice arguments")}
			},
			_ => err!("Invalid slice: {}", val),
		}
	}
	match exp {
		Exp::Tuple(ref args) if args.len() == 2 => {
			let state = build_state(eval_exp(&args[0], ctx));
			let (a, b) = to_slice_params(eval_exp(&args[1], ctx))?;
			Ok(RunVal::State(state.into_iter().chain(::std::iter::repeat(::num::Zero::zero())).skip(a).take(b - a).collect(), Type::Any))
		},
		_ => err!("Invalid `slice` arguments"),
	}
}

fn lib_weighted(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	match exp {
		Exp::Tuple(ref args) => {
			let weights: State = args.iter().map(|arg| {
				let val = eval_exp(arg, ctx);
				if let RunVal::Index(n) = val {Ok(Cf32::new(n as f32, 0_f32))}
				else {err!("Invalid weight: {}", val)}
			}).collect::<Ret<_>>()?;
			let div = weights.iter().fold(Cf32::new(0_f32, 0_f32), |a, b| a + b).sqrt();
			Ok(RunVal::State(weights.into_iter().map(|w| w.sqrt() / div).collect(), Type::Any))
		},
		_ => err!("Invalid `weighted` arguments"),
	}
}

fn lib_fourier(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	match eval_exp(exp, ctx) {
		RunVal::Index(n) if n > 0 => {
			let w = (-2_f32 * ::std::f32::consts::PI * Cf32::i() / n as f32).exp();
			let div = (n as f32).sqrt();
			Ok(RunVal::Gate((0..n)
				.map(|i| (0..n)
					.map(|j| w.powc(Cf32::new((i * j) as f32, 0_f32)) / div)
					.collect())
				.collect()))
		},
		val => err!("Invalid size argument: {}", val),
	}
}

fn lib_repeat(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	fn do_repeat(state: State, n: usize) -> State {
		let div = (n as f32).sqrt();
		(0..n).flat_map(|_| state.iter().map(|s| s / div)).collect()
	}
	match exp {
		&Exp::Tuple(ref args) if args.len() == 2 => {
			let val = eval_exp(&args[0], ctx);
			match eval_exp(&args[1], ctx) {
				RunVal::Index(n) => {
					if let Some(gate) = build_gate(&val, ctx) {
						let wide = gate.into_iter().map(|v| do_repeat(v, n)).collect();
						Ok(RunVal::Gate(::std::iter::repeat(wide).take(n).flat_map(|g: Gate| g).collect()))
					}
					else {Ok(RunVal::State(do_repeat(build_state(val), n), Type::Any))}
				},
				_ => err!("Invalid `repeat` count"),
			}
		},
		_ => err!("Invalid `repeat` arguments"),
	}
}

fn lib_fold(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	match exp {
		&Exp::Tuple(ref args) if args.len() == 3 => {
			let mut current = eval_exp(&args[0], ctx);
			let input = eval_exp(&args[1], ctx);
			let func = eval_exp(&args[2], ctx);
			if let RunVal::Func(fn_ctx_rc, pat, body, _ty) = func {
				let err = Error(format!("Cannot iterate {}", input));
				let list = iterate_val(input).ok_or(err)?;
				for val in list {
					let mut fn_ctx = (*fn_ctx_rc).clone();
					assign_pat(&pat, &RunVal::Tuple(vec![current, val]), &mut fn_ctx)?;
					current = eval_exp(&body, &fn_ctx);
				}
				Ok(current)
			}
			else {
				err!("Invalid `fold` function")
			}
		},
		_ => err!("Invalid `fold` arguments"),
	}
}

fn lib_measure(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	let (s, t) = build_state_typed(eval_exp(exp, ctx))?;
	t.assign(RunVal::Index(s.measure()))
}

fn lib_typeof(exp: &Exp, ctx: &Context) -> Ret<RunVal> {
	Ok(RunVal::String(format!("{}", ::eval_static::infer_type(exp, ctx.types())?)))
}
