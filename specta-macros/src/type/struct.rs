use crate::utils::{parse_attrs, unraw_raw_ident, AttributeValue};
use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{spanned::Spanned, DataStruct, Field, Fields, GenericParam, Generics};

use super::{attr::*, generics::construct_datatype};

pub fn decode_field_attrs(field: &Field) -> syn::Result<FieldAttr> {
    // We pass all the attributes at the start and when decoding them pop them off the list.
    // This means at the end we can check for any that weren't consumed and throw an error.
    let mut attrs = parse_attrs(&field.attrs)?;
    let field_attrs = FieldAttr::from_attrs(&mut attrs)?;

    // The expectation is that when an attribute is processed it will be removed so if any are left over we know they are invalid
    // but we only throw errors for Specta-specific attributes so we don't continually break other attributes.
    if let Some(attrs) = attrs.iter().find(|attr| attr.key == "specta") {
        match &attrs.value {
            Some(AttributeValue::Attribute { attr, .. }) => {
                if let Some(attr) = attr.first() {
                    return Err(syn::Error::new(
                        attr.key.span(),
                        format!("specta: Found unsupported field attribute '{}'", attr.key),
                    ));
                }
            }
            _ => {
                return Err(syn::Error::new(
                    attrs.key.span(),
                    "specta: invalid formatted attribute",
                ))
            }
        }
    }

    Ok(field_attrs)
}

pub fn parse_struct(
    name: &TokenStream,
    container_attrs: &ContainerAttr,
    generics: &Generics,
    crate_ref: &TokenStream,
    sid: &TokenStream,
    data: &DataStruct,
) -> syn::Result<(TokenStream, TokenStream, bool)> {
    let generic_idents = generics
        .params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type(t) => Some(&t.ident),
            _ => None,
        })
        .enumerate()
        .collect::<Vec<_>>();

    let reference_generics = generic_idents.iter().map(|(i, ident)| {
        quote! {
            generics
                .get(#i)
                .cloned()
                .unwrap_or_else(|| <#ident as #crate_ref::Type>::reference(type_map, &[]).inner)
        }
    });
    let reference_generics =
        quote!(let generics: &[#crate_ref::datatype::DataType] = &[#(#reference_generics),*];);

    let reference_generics2 = generic_idents.iter().map(|(i, ident)| {
        let ident_str = ident.to_string();

        quote! {
            (
                std::borrow::Cow::Borrowed(#ident_str).into(),
                generics[#i].clone()
            )
        }
    });

    let definition_generics = generic_idents.iter().map(|(_, ident)| {
        let ident = ident.to_string();
        quote!(std::borrow::Cow::Borrowed(#ident).into())
    });

    let definition = if container_attrs.transparent {
        if let Fields::Unit = data.fields {
            return Err(syn::Error::new(
                data.fields.span(),
                "specta: unit structs cannot be transparent",
            ));
        }

        let (field_ty, field_attrs) = match data.fields {
            Fields::Named(_) => {
                let fields = data
                    .fields
                    .iter()
                    .map(|field| decode_field_attrs(field).map(|v| (field.ty.clone(), v)))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|(_, attrs)| !attrs.skip)
                    .collect::<Vec<_>>();

                if fields.len() != 1 {
                    return Err(syn::Error::new(
                        data.fields.span(),
                        "specta: transparent structs must have exactly one field",
                    ));
                }

                fields.into_iter().next().expect("fields.len() != 1")
            }
            Fields::Unnamed(_) => {
                let fields = data
                    .fields
                    .iter()
                    .map(|field| decode_field_attrs(field).map(|v| (field.ty.clone(), v)))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|(_, attrs)| !attrs.skip)
                    .collect::<Vec<_>>();

                if fields.len() != 1 {
                    return Err(syn::Error::new(
                        data.fields.span(),
                        "specta: transparent structs must have exactly one field",
                    ));
                }

                fields.into_iter().next().expect("fields.len() != 1")
            }
            Fields::Unit => {
                return Err(syn::Error::new(
                    data.fields.span(),
                    "specta: transparent structs must have exactly one field",
                ));
            }
        };

        let field_ty = field_attrs.r#type.as_ref().unwrap_or(&field_ty);

        let ty = construct_datatype(
            format_ident!("ty"),
            field_ty,
            &generic_idents,
            crate_ref,
            field_attrs.inline,
        )?;

        quote!({
            #ty

            ty
        })
    } else {
        let fields = match &data.fields {
            Fields::Named(_) => {
                let fields =
                    data.fields
                        .iter()
                        .map(|field| {
                            let field_attrs = decode_field_attrs(field)?;
                            let field_ty = field_attrs.r#type.as_ref().unwrap_or(&field.ty);

                            let field_ident_str = unraw_raw_ident(field.ident.as_ref().unwrap());

                            let field_name =
                                match (field_attrs.rename.clone(), container_attrs.rename_all) {
                                    (Some(name), _) => name,
                                    (_, Some(inflection)) => {
                                        inflection.apply(&field_ident_str).to_token_stream()
                                    }
                                    (_, _) => field_ident_str.to_token_stream(),
                                };

                            let deprecated = field_attrs.common.deprecated_as_tokens(crate_ref);
                            let optional = field_attrs.optional;
                            let flatten = field_attrs.flatten;
                            let doc = field_attrs.common.doc;

                            let ty = field_attrs.skip.then(|| Ok(quote!(None))).unwrap_or_else(
                                || {
                                    construct_datatype(
                                        format_ident!("ty"),
                                        field_ty,
                                        &generic_idents,
                                        crate_ref,
                                        field_attrs.inline,
                                    )
                                    .map(|ty| {
                                        let ty = if field_attrs.flatten {
                                            quote! {
                                                fn validate_flatten<T: #crate_ref::Flatten>() {}
                                                validate_flatten::<#field_ty>();
                                                #crate_ref::internal::flatten::<#field_ty>(#sid, type_map, &generics)
                                            }
                                        } else {
                                            quote! {
                                                #ty

                                                ty
                                            }
                                        };

                                        quote! {Some({
                                            #ty
                                        })}
                                    })
                                },
                            )?;

                            Ok(
                                quote!((#field_name.into(), #crate_ref::internal::construct::field(
                                    #optional,
                                    #flatten,
                                    #deprecated,
                                    #doc.into(),
                                    #ty
                                ))),
                            )
                        })
                        .collect::<syn::Result<Vec<TokenStream>>>()?;

                let tag = container_attrs
                    .tag
                    .as_ref()
                    .map(|t| quote!(Some(#t.into())))
                    .unwrap_or(quote!(None));

                quote!(#crate_ref::internal::construct::struct_named(vec![#(#fields),*], #tag))
            }
            Fields::Unnamed(_) => {
                let fields = data
                    .fields
                    .iter()
                    .map(|field| {
                        let field_attrs = decode_field_attrs(field)?;
                        let field_ty = field_attrs.r#type.as_ref().unwrap_or(&field.ty);

                        let deprecated = field_attrs.common.deprecated_as_tokens(crate_ref);
                        let optional = field_attrs.optional;
                        let flatten = field_attrs.flatten;
                        let doc = field_attrs.common.doc;

                        let ty = field_attrs.skip.then(|| Ok(quote!(None)))
                            .unwrap_or_else(|| {
                                construct_datatype(
                                    format_ident!("gen"),
                                    field_ty,
                                    &generic_idents,
                                    crate_ref,
                                    field_attrs.inline,
                                ).map(|generic_vars| {
                                    quote! {{
		                               	#generic_vars

		                               	Some(gen)
		                            }}
                                })
                            })?;

                        Ok(quote!(#crate_ref::internal::construct::field(#optional, #flatten, #deprecated, #doc.into(), #ty)))
                    })
                    .collect::<syn::Result<Vec<TokenStream>>>()?;

                quote!(#crate_ref::internal::construct::struct_unnamed(vec![#(#fields),*]))
            }
            Fields::Unit => quote!(#crate_ref::internal::construct::struct_unit()),
        };

        quote!(#crate_ref::datatype::DataType::Struct(#crate_ref::internal::construct::r#struct(#name.into(), Some(#sid), vec![#(#definition_generics),*], #fields)))
    };

    let category = if container_attrs.inline {
        quote!({
            #reference_generics
            #crate_ref::datatype::reference::inline::<Self>(type_map, generics)
        })
    } else {
        quote!({
                #reference_generics
                #crate_ref::datatype::reference::reference::<Self>(type_map, #crate_ref::internal::construct::data_type_reference(
                    #name.into(),
                    #sid,
                    vec![#(#reference_generics2),*]
                ))
        })
    };

    Ok((definition, category, true))
}
